//! Turning this node's own state into a snapshot on disk (OFS-1300 §11).
//!
//! Split from announcing on purpose: this module writes a file and
//! computes the metadata describing it, and the caller signs and gossips
//! that metadata. A snapshot announced before its bytes were durably on
//! disk would be advertised to the cluster while a fetch of it still
//! 404s, so the order — write, then announce — is the whole point of the
//! split.

use crate::codec;
use crate::config::SnapshotConfig;
use crate::error::SnapshotError;
use crate::protocol;
use crate::record::{CompressionMethod, SnapshotId, SnapshotMetadata};
use crate::serve;
use crate::state;
use openfiat_storage::KvStore;
use openfiat_types::{PeerId, PublicKey, Timestamp};
use std::path::{Path, PathBuf};

/// A snapshot that is on disk and described, but not yet announced.
#[derive(Debug, Clone)]
pub struct ProducedSnapshot {
    /// Ready to sign — [`SnapshotMetadata::locations`] is already filled
    /// in from the base URLs the producing node was reachable at.
    pub metadata: SnapshotMetadata,
    pub path: PathBuf,
}

/// Serializes `column_families` out of `store`, writes the compressed
/// result under `config.directory`, prunes older snapshots to
/// `config.retain`, and returns the metadata describing what was written.
///
/// `slot` is this node's local gossip event count at production time —
/// see [`SnapshotMetadata::slot`] for why that is the workspace's
/// answer to a quantity OFS-1300 §9 never defines.
///
/// `base_urls` is where the caller has determined peers can reach this
/// node — [`SnapshotConfig::locations`] computes it from what the node has
/// learned about itself. It is a parameter rather than a config field
/// because it is a *runtime fact* that changes as the node learns its own
/// addresses, and a snapshot must be announced under the addresses that
/// were true when it was written.
///
/// An empty `base_urls` fails before anything is written, rather than
/// producing a file that would be announced with nowhere to fetch it —
/// precisely the state this crate exists to be out of.
pub fn produce<S: KvStore>(
    store: &S,
    column_families: &[&str],
    config: &SnapshotConfig,
    base_urls: &[crate::location::SnapshotLocation],
    slot: u64,
    producer: PeerId,
    producer_public_key: PublicKey,
) -> Result<ProducedSnapshot, SnapshotError> {
    if base_urls.is_empty() {
        return Err(SnapshotError::NoLocationConfigured);
    }

    let created_at = Timestamp::now();
    let id = SnapshotId::new(format!("snap-{slot}-{}", created_at.as_millis()));
    // A duplicate id would be rejected by every peer's index (§24: ids
    // are permanent), so catching it here keeps a node from overwriting a
    // file it is still serving and then announcing into a rejection.
    let path = snapshot_path(&config.directory, &id);
    if path.exists() {
        return Err(SnapshotError::DuplicateSnapshotId);
    }

    // Gzip, so a produced snapshot is a real `.tar.gz` an operator can
    // open with `tar xzf`. `state_root` is taken over the *uncompressed*
    // bytes below, deliberately: the digest has to identify the state, not
    // one particular encoding of it, or re-compressing at a different
    // level would look like different state.
    let compression = CompressionMethod::Gzip;
    let state_bytes = state::serialize(store, column_families)?;
    let state_root = codec::state_root(&state_bytes);
    let compressed = codec::compress(&state_bytes, compression)?;

    // A node whose own state has outgrown what a peer will import must
    // find out here, on its own machine, rather than by writing an hourly
    // file that every peer refuses. Now that a snapshot carries content
    // blocks this is reachable by an archival node on a busy network, and
    // the fix is a shorter retention window.
    if compressed.len() as u64 > codec::MAX_SNAPSHOT_BYTES {
        return Err(SnapshotError::SnapshotTooLarge);
    }

    write_atomically(&path, &compressed)?;
    prune(&config.directory, config.retain);

    let locations = base_urls
        .iter()
        .map(|base| base.join(&serve::download_path(&id)))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(ProducedSnapshot {
        metadata: SnapshotMetadata {
            id,
            snapshot_version: 1,
            protocol_version: protocol::SUPPORTED_PROTOCOL_VERSION,
            slot,
            created_at,
            state_root,
            size_bytes: compressed.len() as u64,
            compression,
            locations,
            producer,
            producer_public_key,
        },
        path,
    })
}

/// The file a snapshot's compressed bytes live in. Shared with
/// [`crate::serve`] so the writer and the reader can never disagree about
/// where a snapshot is.
pub fn snapshot_path(directory: &Path, id: &SnapshotId) -> PathBuf {
    directory.join(format!("{}{}", id.as_str(), serve::FILE_EXTENSION))
}

/// Writes to a sibling temporary file and renames into place. Rename is
/// atomic within a directory on every platform this node runs on, so
/// [`crate::serve`] can never hand a peer a half-written snapshot — the
/// peer would fail the size check and reject a snapshot that is in fact
/// fine, which reads to an operator as a corrupt producer.
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), SnapshotError> {
    let directory = path.parent().ok_or(SnapshotError::StateUnwritable)?;
    std::fs::create_dir_all(directory).map_err(|_| SnapshotError::StateUnwritable)?;
    let temporary = path.with_extension("partial");
    std::fs::write(&temporary, bytes).map_err(|_| SnapshotError::StateUnwritable)?;
    std::fs::rename(&temporary, path).map_err(|_| SnapshotError::StateUnwritable)
}

/// Keeps the `retain` newest snapshots and deletes the rest.
///
/// Best-effort by design: a file that cannot be removed (held open by a
/// download in flight on some platforms, or a permissions change) is left
/// alone and retried next cycle. Failing production because a *cleanup*
/// failed would stop a node snapshotting over a disk-space problem that
/// pruning exists to prevent.
fn prune(directory: &Path, retain: usize) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            // Matched on the whole filename, not `Path::extension()`.
            // The extension is `.tar.gz`, and `extension()` returns only
            // the last component — `gz` — so comparing against it would
            // both miss this node's own files and match any unrelated
            // gzip that happened to be in the directory. Pruning silently
            // matching nothing is the failure that fills a disk.
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(serve::FILE_EXTENSION))
        })
        .collect();
    if files.len() <= retain {
        return;
    }
    // Ids embed slot then creation time, neither zero-padded, so
    // sorting by name would order `snap-9-...` after `snap-10-...`.
    // Modification time is what actually holds.
    files.sort_by_key(|path| {
        let modified = std::fs::metadata(path)
            .and_then(|meta| meta.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        // The name breaks ties: filesystems with coarse timestamps can
        // report two snapshots written seconds apart as simultaneous, and
        // an arbitrary tiebreak there deletes an arbitrary snapshot.
        (modified, path.clone())
    });
    for stale in &files[..files.len() - retain] {
        let _ = std::fs::remove_file(stale);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::location::SnapshotLocation;
    use openfiat_crypto::Keypair;
    use openfiat_network::identity::peer_id_from_public_key;
    use openfiat_storage::mem::MemoryStore;

    fn config(directory: &Path) -> SnapshotConfig {
        SnapshotConfig {
            directory: directory.to_path_buf(),
            retain: 2,
            ..SnapshotConfig::default()
        }
    }

    fn base_urls() -> Vec<SnapshotLocation> {
        vec![SnapshotLocation::parse("http://archive.example:7080").unwrap()]
    }

    fn temporary_directory(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("openfiat-snapshot-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        path
    }

    fn produce_into(store: &MemoryStore, directory: &Path, slot: u64) -> ProducedSnapshot {
        let keypair = Keypair::from_seed([9u8; 32]);
        produce(
            store,
            &["advertisements"],
            &config(directory),
            &base_urls(),
            slot,
            peer_id_from_public_key(&keypair.public_key()).unwrap(),
            keypair.public_key(),
        )
        .unwrap()
    }

    #[test]
    fn a_produced_snapshot_describes_the_file_it_wrote() {
        let directory = temporary_directory("describes");
        let store = MemoryStore::new();
        store.put("advertisements", b"ad-1", b"payload").unwrap();

        let produced = produce_into(&store, &directory, 4217);
        let bytes = std::fs::read(&produced.path).unwrap();
        assert_eq!(produced.metadata.size_bytes, bytes.len() as u64);
        let state_bytes = codec::decompress(&bytes, produced.metadata.compression).unwrap();
        assert_eq!(
            produced.metadata.state_root,
            codec::state_root(&state_bytes)
        );
        assert_eq!(produced.metadata.slot, 4217);
        assert_eq!(
            produced.metadata.locations[0].as_str(),
            format!(
                "http://archive.example:7080/snapshot/{}",
                produced.metadata.id.as_str()
            )
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// A node that has not yet learned where it is reachable writes
    /// nothing. Producing anyway would leave a file on disk announced
    /// under no URL — the undownloadable snapshot this crate exists to
    /// stop.
    #[test]
    fn production_refuses_with_nowhere_to_be_fetched_from() {
        let directory = temporary_directory("no-url");
        let keypair = Keypair::from_seed([9u8; 32]);
        let result = produce(
            &MemoryStore::new(),
            &["advertisements"],
            &config(&directory),
            &[],
            1,
            peer_id_from_public_key(&keypair.public_key()).unwrap(),
            keypair.public_key(),
        );
        assert_eq!(result.unwrap_err(), SnapshotError::NoLocationConfigured);
        assert!(!directory.exists(), "nothing should have been written");
    }

    #[test]
    fn production_prunes_down_to_the_retained_count() {
        let directory = temporary_directory("prune");
        let store = MemoryStore::new();
        for slot in 1..=4 {
            store
                .put("advertisements", format!("ad-{slot}").as_bytes(), b"x")
                .unwrap();
            produce_into(&store, &directory, slot);
        }
        let remaining = std::fs::read_dir(&directory).unwrap().count();
        assert_eq!(remaining, 2, "retain: 2");
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// The shipped default keeps exactly one, and it is the newest.
    ///
    /// Counting the files is not enough on its own — a prune that kept the
    /// *oldest* would also leave one behind, and would leave every joining
    /// node fetching state that gets staler every cycle while the node
    /// reports itself as producing normally.
    #[test]
    fn the_default_keeps_only_the_newest_snapshot() {
        let directory = temporary_directory("prune-default");
        let store = MemoryStore::new();
        let mut newest = None;
        for slot in 1..=3 {
            store
                .put("advertisements", format!("ad-{slot}").as_bytes(), b"x")
                .unwrap();
            let keypair = Keypair::from_seed([9u8; 32]);
            let config = SnapshotConfig {
                directory: directory.clone(),
                retain: crate::config::DEFAULT_RETAIN,
                ..SnapshotConfig::default()
            };
            newest = Some(
                produce(
                    &store,
                    &["advertisements"],
                    &config,
                    &base_urls(),
                    slot,
                    peer_id_from_public_key(&keypair.public_key()).unwrap(),
                    keypair.public_key(),
                )
                .unwrap(),
            );
            // Modification time is what prune orders by, and a loop this
            // tight can write two files inside one filesystem tick.
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        let files: Vec<_> = std::fs::read_dir(&directory)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .collect();
        assert_eq!(files.len(), 1, "the default retains one");
        assert_eq!(
            files[0],
            newest.unwrap().path,
            "the one kept must be the newest, not merely one of them"
        );
        let _ = std::fs::remove_dir_all(&directory);
    }
}
