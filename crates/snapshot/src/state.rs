//! What a snapshot actually contains: this node's persisted state, taken
//! at the only layer that sees all of it at once — the key/value store
//! every domain registry writes through.
//!
//! Snapshotting at the `KvStore` layer rather than per-registry keeps
//! [`crate::record`]'s original promise intact for the *meaning* of the
//! bytes: this crate still knows nothing about advertisements, disputes,
//! or governance, only about column families, keys, and opaque values. It
//! also means a new domain crate is snapshotted the moment its column
//! family joins the list, with no change here — the alternative, a
//! hand-maintained per-registry serializer, silently omits whatever
//! nobody remembered to add, and a snapshot that silently omits state is
//! exactly the failure this crate exists to prevent.
//!
//! **The encoding is canonical.** Column families and the keys within
//! them are sorted before encoding, so two honest nodes holding identical
//! state produce byte-identical snapshots and therefore an identical
//! `state_root`. Without that, the state root would depend on RocksDB
//! iteration order and no second node could ever confirm a producer's
//! work.

use crate::error::SnapshotError;
use openfiat_serialization::wire;
use openfiat_storage::{Entry, KvStore};

/// One column family's full contents: its name, and every `(key, value)`
/// pair in it sorted by key.
pub type ColumnFamilySnapshot = (String, Vec<Entry>);

/// The on-the-wire body of a snapshot. Sorted at construction, never
/// after — see the module doc on why the ordering is load-bearing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StateSnapshot {
    /// Sorted by column family name.
    pub column_families: Vec<ColumnFamilySnapshot>,
}

impl StateSnapshot {
    pub fn entry_count(&self) -> usize {
        self.column_families
            .iter()
            .map(|(_, entries)| entries.len())
            .sum()
    }
}

/// Reads `column_families` out of `store` into a canonical
/// [`StateSnapshot`] and encodes it.
///
/// A column family that cannot be read is an error, not an omission: a
/// snapshot missing a registry would be silently and permanently wrong
/// for every node that imported it.
pub fn serialize<S: KvStore>(
    store: &S,
    column_families: &[&str],
) -> Result<Vec<u8>, SnapshotError> {
    let mut names: Vec<&str> = column_families.to_vec();
    names.sort_unstable();
    names.dedup();

    let mut families = Vec::with_capacity(names.len());
    for name in names {
        let mut entries = store
            .iter_prefix(name, &[])
            .map_err(|_| SnapshotError::StateUnreadable)?;
        entries.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));
        families.push((name.to_string(), entries));
    }

    wire::to_bytes(&StateSnapshot {
        column_families: families,
    })
    .map_err(|_| SnapshotError::MalformedRecord)
}

/// Whether one entry may be written into the importing node's store.
///
/// # Why this crate takes a predicate instead of deciding
///
/// A snapshot is an untrusted file from a peer. The state root proves the
/// blob is the one the producer announced and signed — it says nothing at
/// all about whether the producer put honest contents inside it, because
/// the producer computed the root over whatever they assembled.
///
/// For most column families there is nothing to check: a record is
/// whatever the network's registries say it is, and a producer able to
/// forge one could equally have gossiped it. But some column families
/// have a *self-describing* invariant — content addressed by its own
/// hash, most obviously — where an entry can be checked against its own
/// key by anyone, and where accepting a bad one means this node serves
/// somebody else's bytes under a trusted identifier, to challengers and
/// to browsers.
///
/// This crate deliberately knows no domain, so it cannot perform that
/// check; the node that composes the column families supplies it.
/// [`accept_any`] is what "there is nothing here to check" looks like
/// when written down.
pub type EntryVerifier = fn(column_family: &str, key: &[u8], value: &[u8]) -> bool;

/// Accepts every entry.
///
/// Correct for a store whose column families hold only records —
/// entries whose contents no importer can independently judge, so
/// refusing one would only mean refusing state that a re-gossip would
/// deliver anyway. Not correct for a store holding self-verifying data;
/// see [`EntryVerifier`], and pass this only when the answer is genuinely
/// "there is nothing to check".
pub fn accept_any(_column_family: &str, _key: &[u8], _value: &[u8]) -> bool {
    true
}

/// Writes a decoded snapshot into `store`, returning how many entries
/// landed.
///
/// Decoding, the `reserved` check and `verify` all happen in full before
/// the first write, so a malformed blob — one reaching for a column
/// family it may not touch, or carrying one entry the importer will not
/// stand behind — leaves the store untouched rather than half-replaced.
/// The caller is responsible for having verified `state_root` first;
/// [`crate::store::SnapshotIndex::import`] is the only path that should
/// call this, and it verifies before it does.
///
/// `reserved` names the column families a snapshot may never write. The
/// importing node's own checkpoint and its index of verified
/// announcements belong to it alone: a producer able to set an importer's
/// checkpoint to `u64::MAX` would permanently lock that node out of every
/// future import, from a snapshot that otherwise verified perfectly. A
/// signed, registry-authorized producer is trusted for *state*, not for
/// the bookkeeping that decides what this node imports next.
///
/// `verify` is the importer's own opinion of the contents — see
/// [`EntryVerifier`]. One rejected entry fails the whole snapshot rather
/// than being skipped: a snapshot with a hole in it is not a smaller
/// snapshot, it is state whose state root no longer describes what is on
/// disk.
pub fn restore<S: KvStore>(
    store: &S,
    bytes: &[u8],
    reserved: &[&str],
    verify: EntryVerifier,
) -> Result<usize, SnapshotError> {
    let snapshot: StateSnapshot =
        wire::from_bytes(bytes).map_err(|_| SnapshotError::MalformedRecord)?;
    if snapshot
        .column_families
        .iter()
        .any(|(name, _)| reserved.contains(&name.as_str()))
    {
        return Err(SnapshotError::ReservedColumnFamily);
    }
    for (name, entries) in &snapshot.column_families {
        if !entries.iter().all(|(key, value)| verify(name, key, value)) {
            return Err(SnapshotError::UnverifiableEntry);
        }
    }

    let mut restored = 0;
    for (name, entries) in &snapshot.column_families {
        for (key, value) in entries {
            store
                .put(name, key, value)
                .map_err(|_| SnapshotError::StateUnwritable)?;
            restored += 1;
        }
    }
    Ok(restored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfiat_storage::mem::MemoryStore;

    #[test]
    fn a_snapshot_round_trips_into_an_empty_store() {
        let source = MemoryStore::new();
        source.put("advertisements", b"ad-1", b"first").unwrap();
        source.put("disputes", b"dis-1", b"second").unwrap();

        let bytes = serialize(&source, &["advertisements", "disputes"]).unwrap();
        let target = MemoryStore::new();
        assert_eq!(restore(&target, &bytes, &[], accept_any).unwrap(), 2);
        assert_eq!(
            target.get("advertisements", b"ad-1").unwrap(),
            Some(b"first".to_vec())
        );
        assert_eq!(
            target.get("disputes", b"dis-1").unwrap(),
            Some(b"second".to_vec())
        );
    }

    /// The state root is only meaningful if two nodes holding the same
    /// state agree on the bytes, whatever order they wrote them in.
    #[test]
    fn insertion_order_does_not_change_the_encoding() {
        let forwards = MemoryStore::new();
        forwards.put("advertisements", b"a", b"1").unwrap();
        forwards.put("advertisements", b"b", b"2").unwrap();
        forwards.put("disputes", b"c", b"3").unwrap();

        let backwards = MemoryStore::new();
        backwards.put("disputes", b"c", b"3").unwrap();
        backwards.put("advertisements", b"b", b"2").unwrap();
        backwards.put("advertisements", b"a", b"1").unwrap();

        let cfs = ["advertisements", "disputes"];
        assert_eq!(
            serialize(&forwards, &cfs).unwrap(),
            serialize(&backwards, &cfs).unwrap()
        );
    }

    /// Same reasoning, for the caller's list rather than the store's
    /// contents.
    #[test]
    fn the_column_family_argument_order_does_not_change_the_encoding() {
        let store = MemoryStore::new();
        store.put("advertisements", b"a", b"1").unwrap();
        store.put("disputes", b"c", b"3").unwrap();
        assert_eq!(
            serialize(&store, &["advertisements", "disputes"]).unwrap(),
            serialize(&store, &["disputes", "advertisements"]).unwrap()
        );
    }

    #[test]
    fn a_truncated_blob_is_rejected_without_writing_anything() {
        let source = MemoryStore::new();
        source.put("advertisements", b"ad-1", b"first").unwrap();
        let bytes = serialize(&source, &["advertisements"]).unwrap();

        let target = MemoryStore::new();
        assert_eq!(
            restore(&target, &bytes[..bytes.len() / 2], &[], accept_any),
            Err(SnapshotError::MalformedRecord)
        );
        assert_eq!(target.get("advertisements", b"ad-1").unwrap(), None);
    }

    /// A snapshot's state root proves the blob is what the producer
    /// announced. It does not prove the producer put honest contents
    /// inside it — the root was computed over whatever they assembled —
    /// so an entry the importer can judge for itself is judged.
    #[test]
    fn one_entry_the_importer_will_not_stand_behind_fails_the_whole_snapshot() {
        /// Stands in for the real thing: a column family whose values
        /// must equal their own keys, which is the shape of anything
        /// content-addressed.
        fn value_must_equal_key(family: &str, key: &[u8], value: &[u8]) -> bool {
            family != "self_describing" || key == value
        }

        let source = MemoryStore::new();
        source
            .put("advertisements", b"ad-1", b"legitimate")
            .unwrap();
        source.put("self_describing", b"honest", b"honest").unwrap();
        source
            .put("self_describing", b"claims-to-be", b"is-not")
            .unwrap();
        let bytes = serialize(&source, &["advertisements", "self_describing"]).unwrap();

        let target = MemoryStore::new();
        assert_eq!(
            restore(&target, &bytes, &[], value_must_equal_key),
            Err(SnapshotError::UnverifiableEntry)
        );
        // Not one entry of it, including the entries that were fine: a
        // snapshot with a hole in it is state whose root no longer
        // describes what is on disk.
        assert_eq!(target.get("advertisements", b"ad-1").unwrap(), None);
        assert_eq!(target.get("self_describing", b"honest").unwrap(), None);
    }

    #[test]
    fn a_snapshot_whose_entries_all_pass_is_written_in_full() {
        // The other half of the check above: a verifier that refuses
        // everything would pass that test and break every import.
        fn value_must_equal_key(family: &str, key: &[u8], value: &[u8]) -> bool {
            family != "self_describing" || key == value
        }

        let source = MemoryStore::new();
        source.put("self_describing", b"honest", b"honest").unwrap();
        let bytes = serialize(&source, &["self_describing"]).unwrap();

        let target = MemoryStore::new();
        assert_eq!(restore(&target, &bytes, &[], value_must_equal_key), Ok(1));
    }

    /// A producer that could write the importer's checkpoint could lock
    /// it out of every future import — so the guard is checked before any
    /// write, not per entry.
    #[test]
    fn a_snapshot_reaching_for_a_reserved_column_family_writes_nothing() {
        let hostile = MemoryStore::new();
        hostile
            .put("advertisements", b"ad-1", b"legitimate")
            .unwrap();
        hostile
            .put("snapshot_checkpoint", b"local", &u64::MAX.to_le_bytes())
            .unwrap();
        let bytes = serialize(&hostile, &["advertisements", "snapshot_checkpoint"]).unwrap();

        let target = MemoryStore::new();
        assert_eq!(
            restore(&target, &bytes, &["snapshot_checkpoint"], accept_any),
            Err(SnapshotError::ReservedColumnFamily)
        );
        assert_eq!(target.get("advertisements", b"ad-1").unwrap(), None);
    }
}
