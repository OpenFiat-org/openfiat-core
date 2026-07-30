//! `openfiat-crypto` — Cryptographic primitives: signing, hashing, key derivation.
//!
//! Ed25519 for signing (matches OFNP §6/ONSP §5's "Public key / Private
//! key" node identity model), SHA-256 for content hashing — including
//! [`cid`], the IPFS content identifier that names a hash of data held
//! outside the protocol — and X25519 +
//! ChaCha20-Poly1305 sealed boxes ([`seal`]) for the one thing signing
//! cannot do: addressing a secret to a single peer's published identity
//! key. Nothing here ever exposes raw private key bytes outside
//! [`keypair::Keypair`] itself.
//!
//! [`challenge`] sits one level up from those primitives: it is the
//! sign-this-nonce handshake that turns "I hold this key" into an
//! answerable question, which is the only form of authentication a
//! protocol with no accounts can offer.

pub mod challenge;
pub mod cid;
pub mod hash;
pub mod keypair;
pub mod seal;
pub mod verify;

pub use challenge::{CHALLENGE_TTL_SECS, Challenge, ChallengeError, ChallengeLedger};
pub use cid::{Cid, CidError};
pub use hash::sha256;
pub use keypair::Keypair;
pub use seal::{SealError, SealedBox, open, seal};
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
