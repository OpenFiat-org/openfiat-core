//! §10's State Root and §15's compression — see `record::CompressionMethod`
//! for why only `None` is actually implemented today.

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
/// Every stage of the pipeline holds the whole thing in memory: the
/// producer serializes into a `Vec`, the fetcher downloads into one, the
/// importer decompresses into another. So this is a memory bound, not a
/// policy about how much content is worth keeping. Two gibibytes is the
/// largest a node with a few gigabytes of RAM can move through that
/// pipeline without the import being the thing that kills it.
///
/// A producer whose state exceeds this fails to produce, loudly, rather
/// than writing a file every peer would refuse. The fix is a shorter
/// `--retention`: the node keeps serving what it holds either way, and
/// what it stops doing is handing its whole archive to newcomers in one
/// blob.
pub const MAX_SNAPSHOT_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// §10: "a cryptographic digest representing the complete snapshot state."
pub fn state_root(state_bytes: &[u8]) -> [u8; 32] {
    sha256(state_bytes)
}

pub fn compress(state_bytes: &[u8], method: CompressionMethod) -> Result<Vec<u8>, SnapshotError> {
    match method {
        CompressionMethod::None => Ok(state_bytes.to_vec()),
        CompressionMethod::Zstd | CompressionMethod::Gzip => {
            Err(SnapshotError::UnsupportedCompression)
        }
    }
}

pub fn decompress(compressed: &[u8], method: CompressionMethod) -> Result<Vec<u8>, SnapshotError> {
    match method {
        CompressionMethod::None => Ok(compressed.to_vec()),
        CompressionMethod::Zstd | CompressionMethod::Gzip => {
            Err(SnapshotError::UnsupportedCompression)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
