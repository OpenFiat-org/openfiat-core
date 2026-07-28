//! §10's State Root and §15's compression — see `record::CompressionMethod`
//! for why only `None` is actually implemented today.

use crate::error::SnapshotError;
use crate::record::CompressionMethod;
use openfiat_crypto::hash::sha256;

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
