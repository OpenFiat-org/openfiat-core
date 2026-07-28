//! Ed25519 keypair generation and signing.
//!
//! The signing key never leaves this module: everything downstream only
//! ever sees a [`openfiat_types::PublicKey`] or a [`openfiat_types::Signature`]
//! it produced, matching ONSP §5's "Private keys MUST never be transmitted
//! across the network".

use ed25519_dalek::{Signer, SigningKey};
use openfiat_types::{PublicKey, Signature};
use rand::SeedableRng;
use rand::rngs::{StdRng, SysRng};

/// An Ed25519 keypair capable of signing messages on this node's behalf.
pub struct Keypair(SigningKey);

impl Keypair {
    /// Generate a new keypair from the operating system's CSPRNG.
    ///
    /// `SysRng` only implements rand's fallible RNG trait, and
    /// `ed25519_dalek::SigningKey::generate` requires the infallible
    /// `CryptoRng` — so a `StdRng` is seeded from `SysRng` once, then used
    /// as the actual generator, per `rand`'s own recommended pattern for
    /// this RNG.
    ///
    /// # Panics
    /// Panics if the operating system's entropy source is unavailable.
    pub fn generate() -> Self {
        let mut rng = StdRng::try_from_rng(&mut SysRng).expect("OS entropy source unavailable");
        Self(SigningKey::generate(&mut rng))
    }

    /// Reconstruct a keypair from a previously generated 32-byte seed.
    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self(SigningKey::from_bytes(&seed))
    }

    /// The 32-byte seed this keypair was generated from, for persistence.
    pub fn seed(&self) -> [u8; 32] {
        self.0.to_bytes()
    }

    /// This keypair's public half.
    pub fn public_key(&self) -> PublicKey {
        PublicKey::from_bytes(self.0.verifying_key().to_bytes())
    }

    /// Sign a message, producing a [`Signature`] verifiable against
    /// [`Keypair::public_key`].
    pub fn sign(&self, message: &[u8]) -> Signature {
        Signature::from_bytes(self.0.sign(message).to_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_keypairs_are_not_the_same() {
        assert_ne!(Keypair::generate().public_key(), Keypair::generate().public_key());
    }

    #[test]
    fn from_seed_is_deterministic() {
        let seed = [42u8; 32];
        assert_eq!(Keypair::from_seed(seed).public_key(), Keypair::from_seed(seed).public_key());
    }
}
