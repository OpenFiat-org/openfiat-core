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
//! [`encryption_key`] is the answer to the question [`seal`] raises and
//! cannot answer: a sealed box addressed to an Ed25519 key can only be
//! opened by something holding that key's secret, and a browser wallet
//! holds none it will part with. So a wallet derives a *separate* X25519
//! key from a signature over one fixed message and publishes the public
//! half as an identity claim. That module states what the arrangement
//! costs as carefully as what it buys.
//!
//! [`challenge`] sits one level up from those primitives: it is the
//! sign-this-nonce handshake that turns "I hold this key" into an
//! answerable question, which is the only form of authentication a
//! protocol with no accounts can offer.

pub mod challenge;
pub mod cid;
pub mod encryption_key;
pub mod hash;
pub mod keypair;
pub mod mint;
pub mod seal;
pub mod verify;

pub use challenge::{CHALLENGE_TTL_SECS, Challenge, ChallengeError, ChallengeLedger};
pub use cid::{Cid, CidError};
pub use encryption_key::{
    DERIVATION_MESSAGE, EncryptionKeyError, EncryptionKeypair, EncryptionPublicKey,
};
pub use hash::sha256;
pub use keypair::Keypair;
pub use mint::{MintAddress, MintError};
pub use seal::{SealError, SealedBox, open, open_x25519, seal, seal_to_x25519};
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
