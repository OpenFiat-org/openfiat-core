//! §10's State Root and §15's compression.
//!
//! # Why a snapshot is a `.tar.gz` and not just a `.gz`
//!
//! The compressed payload is a real tar archive holding one member,
//! `state`. A single gzip stream would be smaller by the 1 KiB a tar
//! header and its padding cost, and it would also be a format only this
//! implementation can open.
//!
//! The tar buys three things that are worth more than a kilobyte. An
//! operator can run `tar xzf` on a downloaded snapshot and look at what
//! their node is about to import, which is the difference between a
//! format and a black box. It matches what a Solana operator already
//! expects, since Solana's own snapshots are `.tar.zst`. And it leaves
//! room for a second member — content blocks separately from records,
//! say — without another format change, because a reader that walks
//! entries by name ignores what it does not recognise.
//!
//! Members are read by name rather than by position, so adding one is
//! backwards compatible in the direction that matters: an old reader
//! skips it.

use crate::error::SnapshotError;
use crate::record::CompressionMethod;
use openfiat_crypto::hash::sha256;

/// The largest snapshot this node will produce, download, or import.
///
/// # Why there has to be one now
///
/// A snapshot used to be records only — a few megabytes of advertisements
/// and settlements, where `size_bytes` from a signed announcement was
/// bound enough. Now it carries the content blocks a node holds, so its
/// size follows real trading volume and the operator's retention window,
/// and both of those are numbers somebody else chose.
///
/// This is a memory bound, not a policy about how much content is worth
/// keeping — and the memory it bounds is not one copy of the snapshot but
/// three, held at the same moment by the *importer*, which is the node
/// that gets no say in how large the file is:
///
/// 1. the compressed blob, owned by `fetch::download` for the whole of
///    the import it then calls;
/// 2. the decompressed state, which [`decompress`] returns while (1) is
///    still alive;
/// 3. the decoded `StateSnapshot`, which `state::restore` deserializes
///    out of (2) into owned `Vec`s while (2) is still alive.
///
/// (2) is not smaller than (1) in the case that matters. A snapshot's
/// bulk is content blocks, and an attachment is a photographed receipt or
/// a PDF — bytes that are already compressed and that gzip does not
/// shrink again. So the peak is close to three times this constant.
///
/// Half a gibibyte is therefore the largest a node with a few gigabytes
/// of RAM can move through that pipeline without the import being the
/// thing that kills it. It was two gibibytes, on the same "a few
/// gigabytes of RAM" reasoning, which had counted one copy where there
/// are three: a node accepting the old ceiling could be asked to allocate
/// six gigabytes to finish bootstrapping, and would be OOM-killed part
/// way through by a snapshot that verified perfectly.
///
/// A producer whose state exceeds this fails to produce, loudly, rather
/// than writing a file every peer would refuse. The fix is a shorter
/// `--retention`: the node keeps serving what it holds either way, and
/// what it stops doing is handing its whole archive to newcomers in one
/// blob.
pub const MAX_SNAPSHOT_BYTES: u64 = 512 * 1024 * 1024;

/// §10: "a cryptographic digest representing the complete snapshot state."
pub fn state_root(state_bytes: &[u8]) -> [u8; 32] {
    sha256(state_bytes)
}

/// The tar member the serialized state is stored under.
///
/// Read by name, never by position — see this module's own docs for why
/// that is what makes a second member possible later.
const STATE_MEMBER: &str = "state";

pub fn compress(state_bytes: &[u8], method: CompressionMethod) -> Result<Vec<u8>, SnapshotError> {
    match method {
        CompressionMethod::None => Ok(state_bytes.to_vec()),
        CompressionMethod::Gzip => gzip_tar(state_bytes),
        // Left unimplemented rather than quietly aliased to gzip: a
        // producer that announces Zstd and ships gzip is worse than one
        // that refuses, because the mismatch surfaces on the consumer.
        CompressionMethod::Zstd => Err(SnapshotError::UnsupportedCompression),
    }
}

pub fn decompress(compressed: &[u8], method: CompressionMethod) -> Result<Vec<u8>, SnapshotError> {
    match method {
        // Still accepted, and must stay so: announcements naming `None`
        // are already in flight, and a node that refused them would drop
        // every snapshot produced before this change.
        CompressionMethod::None => Ok(compressed.to_vec()),
        CompressionMethod::Gzip => ungzip_tar(compressed),
        CompressionMethod::Zstd => Err(SnapshotError::UnsupportedCompression),
    }
}

fn gzip_tar(state_bytes: &[u8]) -> Result<Vec<u8>, SnapshotError> {
    use std::io::Write;

    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut archive = tar::Builder::new(encoder);

    let mut header = tar::Header::new_gnu();
    header.set_size(state_bytes.len() as u64);
    header.set_mode(0o644);
    // A fixed mtime, deliberately. Two nodes snapshotting identical state
    // should produce identical bytes; a wall-clock timestamp in the header
    // would make the archive differ every time and defeat any comparison
    // of one producer's output against another's.
    header.set_mtime(0);
    header.set_cksum();

    archive
        .append_data(&mut header, STATE_MEMBER, state_bytes)
        .map_err(|_| SnapshotError::UnsupportedCompression)?;
    archive
        .into_inner()
        .and_then(|encoder| encoder.finish())
        .and_then(|mut buffer| {
            buffer.flush()?;
            Ok(buffer)
        })
        .map_err(|_| SnapshotError::UnsupportedCompression)
}

fn ungzip_tar(compressed: &[u8]) -> Result<Vec<u8>, SnapshotError> {
    use std::io::Read;

    let decoder = flate2::read::GzDecoder::new(compressed);
    let mut archive = tar::Archive::new(decoder);
    let entries = archive
        .entries()
        .map_err(|_| SnapshotError::UnsupportedCompression)?;

    for entry in entries {
        let entry = entry.map_err(|_| SnapshotError::UnsupportedCompression)?;
        let is_state = entry
            .path()
            .map(|path| path.to_string_lossy() == STATE_MEMBER)
            .unwrap_or(false);
        if !is_state {
            continue;
        }

        // Bounded by the same limit the whole pipeline is bounded by, so
        // a hostile archive claiming a petabyte cannot be used to exhaust
        // memory here — decompression is exactly where that would be
        // attempted, since the compressed size says nothing about it.
        let mut state = Vec::new();
        entry
            .take(MAX_SNAPSHOT_BYTES)
            .read_to_end(&mut state)
            .map_err(|_| SnapshotError::UnsupportedCompression)?;
        return Ok(state);
    }

    Err(SnapshotError::UnsupportedCompression)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gzip_round_trips_the_exact_state_bytes() {
        for size in [0usize, 1, 1024, 200_000] {
            let state: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
            let compressed = compress(&state, CompressionMethod::Gzip).unwrap();
            assert_eq!(
                decompress(&compressed, CompressionMethod::Gzip).unwrap(),
                state,
                "{size} bytes did not survive the round trip"
            );
        }
    }

    /// The point of the format: `tar xzf` opens it. Checked by reading the
    /// archive the way any other tool would rather than by calling this
    /// module's own reader, which would pass even if the bytes were some
    /// private encoding.
    #[test]
    fn the_output_is_a_gzip_stream_containing_a_tar_with_one_named_member() {
        let state = b"records and content blocks".to_vec();
        let compressed = compress(&state, CompressionMethod::Gzip).unwrap();

        // The gzip magic, which is what `file(1)` and every archive tool
        // dispatch on.
        assert_eq!(&compressed[..2], &[0x1f, 0x8b], "not a gzip stream");

        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(&compressed[..]));
        let members: Vec<(String, Vec<u8>)> = archive
            .entries()
            .unwrap()
            .map(|entry| {
                use std::io::Read;
                let mut entry = entry.unwrap();
                let name = entry.path().unwrap().to_string_lossy().into_owned();
                let mut body = Vec::new();
                entry.read_to_end(&mut body).unwrap();
                (name, body)
            })
            .collect();

        assert_eq!(members, vec![(STATE_MEMBER.to_string(), state)]);
    }

    /// Two nodes snapshotting identical state must produce identical
    /// bytes. A tar header carries an mtime, and letting it default to the
    /// wall clock would make every archive of the same state differ.
    #[test]
    fn compressing_the_same_state_twice_gives_the_same_bytes() {
        let state = b"deterministic".to_vec();
        assert_eq!(
            compress(&state, CompressionMethod::Gzip).unwrap(),
            compress(&state, CompressionMethod::Gzip).unwrap()
        );
    }

    /// Announcements naming `None` are already in flight. A node that
    /// refused them would drop every snapshot produced before compression
    /// existed, which is a network partition dressed as a format upgrade.
    #[test]
    fn uncompressed_snapshots_are_still_importable() {
        let state = b"produced before this change".to_vec();
        assert_eq!(decompress(&state, CompressionMethod::None).unwrap(), state);
    }

    #[test]
    fn a_gzip_stream_that_is_not_a_tar_is_refused_rather_than_returned_raw() {
        use std::io::Write;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(b"gzipped, but not an archive").unwrap();
        let bare_gzip = encoder.finish().unwrap();

        assert!(decompress(&bare_gzip, CompressionMethod::Gzip).is_err());
    }

    #[test]
    fn state_root_is_deterministic() {
        assert_eq!(state_root(b"hello"), state_root(b"hello"));
        assert_ne!(state_root(b"hello"), state_root(b"world"));
    }

    #[test]
    fn none_compression_round_trips() {
        let compressed = compress(b"some state", CompressionMethod::None).unwrap();
        assert_eq!(
            decompress(&compressed, CompressionMethod::None).unwrap(),
            b"some state"
        );
    }

    #[test]
    fn zstd_is_rejected_as_unsupported() {
        assert_eq!(
            compress(b"x", CompressionMethod::Zstd),
            Err(SnapshotError::UnsupportedCompression)
        );
        assert_eq!(
            decompress(b"x", CompressionMethod::Gzip),
            Err(SnapshotError::UnsupportedCompression)
        );
    }
}
