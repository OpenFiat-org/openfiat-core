//! Content hashing.
//!
//! Just the primitive: which bytes get hashed to form e.g. an
//! `openfiat_types::EventId` (canonical wire encoding minus the signature
//! field, per OGP) is domain logic that belongs to the crate constructing
//! that ID, not here.

use sha2::{Digest, Sha256};

/// The SHA-256 digest of `bytes`.
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_deterministic() {
        assert_eq!(sha256(b"openfiat"), sha256(b"openfiat"));
    }

    #[test]
    fn differs_for_different_input() {
        assert_ne!(sha256(b"openfiat"), sha256(b"OpenFiat"));
    }

    #[test]
    fn matches_a_known_test_vector() {
        // sha256("") — a standard vector, catches a wrong hasher/encoding.
        let hex: String = sha256(b"").iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }
}
