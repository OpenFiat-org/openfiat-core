//! `openfiat-crypto` — Cryptographic primitives: signing, hashing, key derivation.
//!
//! Ed25519 for signing (matches OFNP §6/ONSP §5's "Public key / Private
//! key" node identity model), SHA-256 for content hashing. Nothing here
//! ever exposes raw private key bytes outside [`keypair::Keypair`] itself.

pub mod hash;
pub mod keypair;
pub mod verify;

pub use hash::sha256;
pub use keypair::Keypair;
pub use verify::{VerifyError, verify};

/// Crate version, re-exported for diagnostics and `openfiat-node --version`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_a_version() {
        assert!(!version().is_empty());
    }
}
