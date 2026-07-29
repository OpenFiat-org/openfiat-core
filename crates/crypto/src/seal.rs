//! Sealed boxes — anonymous public-key authenticated encryption addressed
//! to the holder of an Ed25519 [`openfiat_types::PublicKey`].
//!
//! This exists because OFS-6000 §11 subscriptions are gossip-replicated to
//! *every* node on the network. A wallet's delivery destination (an email
//! address, a phone number, a chat ID) must therefore never travel as
//! plaintext inside one: doing so would broadcast every user's contact
//! details to the whole network, permanently, into a replicated store.
//! §19's rule that a provider receives "only what delivery requires" is
//! only enforceable if the destination is readable by exactly one party —
//! the gateway the wallet bound it to.
//!
//! Construction (libsodium's `crypto_box_seal` shape, RustCrypto parts):
//!
//! 1. A fresh ephemeral X25519 keypair per seal, so no two seals to the
//!    same recipient share a key stream and the *sender* stays anonymous.
//! 2. The recipient's Ed25519 verifying key is mapped to its birationally
//!    equivalent Montgomery (X25519) form, and the two are combined by
//!    ECDH.
//! 3. The AEAD key and nonce are derived from SHA-256 over domain-separated
//!    transcripts that commit to *both* public keys as well as the shared
//!    secret, so a seal is cryptographically bound to its intended
//!    recipient.
//! 4. ChaCha20-Poly1305 encrypts the plaintext with the ephemeral public
//!    key as associated data, so swapping in a different ephemeral key
//!    fails authentication rather than silently decrypting to garbage.
//!
//! Reusing a long-term signing key for key exchange is a documented
//! trade-off (see `ed25519_dalek::VerifyingKey::to_montgomery`'s own
//! note). It is deliberate here: a `ServiceRecord` already carries a
//! gateway's `provider_public_key` and nothing else, so a wallet can
//! address a gateway today with no new registration field, no key
//! distribution step, and no chance of sealing to a key nobody can prove
//! ownership of.

use crate::keypair::Keypair;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use curve25519_dalek::montgomery::MontgomeryPoint;
use curve25519_dalek::traits::IsIdentity;
use ed25519_dalek::VerifyingKey;
use openfiat_types::{ErrorCode, PublicKey};
use rand::rngs::{StdRng, SysRng};
use rand::{Rng, SeedableRng};
use sha2::{Digest, Sha256};
use std::fmt;

/// Domain separator for the AEAD key derivation. Changing it changes the
/// wire format: old sealed boxes stop opening.
const KEY_DOMAIN: &[u8] = b"openfiat/sealedbox/v1/key";
/// Domain separator for the nonce derivation, distinct from `KEY_DOMAIN`
/// so the two never collapse to the same digest.
const NONCE_DOMAIN: &[u8] = b"openfiat/sealedbox/v1/nonce";

/// A ciphertext only the recipient's private key can open, plus the
/// ephemeral public key needed to derive the opening key.
///
/// Safe to replicate over gossip: it carries no sender identity and no
/// recipient-identifying material beyond what the addressing already
/// implies.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SealedBox {
    /// The per-seal ephemeral X25519 public key, in Montgomery form.
    pub ephemeral_public: [u8; 32],
    pub nonce: [u8; 12],
    pub ciphertext: Vec<u8>,
}

/// Sealing or opening failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SealError {
    /// The recipient's public key is not a valid Ed25519 point, or the
    /// key exchange with it degenerated to the identity element (a
    /// small-order key). Only ever reported by [`seal`], where the
    /// offending key is public input anyway.
    InvalidRecipientKey,
    /// The sealed box did not open. Deliberately collapses "wrong
    /// recipient", "tampered ciphertext", and "tampered ephemeral key"
    /// into one variant, for the same reason [`crate::VerifyError`]
    /// does: telling them apart is an oracle.
    Failed,
}

impl SealError {
    /// The OFS-8000 code this failure maps to.
    pub const fn code(self) -> ErrorCode {
        match self {
            Self::InvalidRecipientKey => ErrorCode::InvalidParameter,
            Self::Failed => ErrorCode::InvalidSignature,
        }
    }
}

impl fmt::Display for SealError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRecipientKey => write!(f, "recipient public key is unusable for sealing"),
            Self::Failed => write!(f, "sealed box did not open"),
        }
    }
}

impl std::error::Error for SealError {}

/// Encrypt `plaintext` so that only the holder of `recipient`'s private
/// key can read it. Every call uses a fresh ephemeral key, so sealing the
/// same plaintext twice produces two unrelated ciphertexts.
///
/// # Panics
/// Panics if the operating system's entropy source is unavailable — the
/// same stance [`Keypair::generate`] takes, since silently sealing under
/// predictable randomness would be worse than not running.
pub fn seal(recipient: &PublicKey, plaintext: &[u8]) -> Result<SealedBox, SealError> {
    let recipient_point = montgomery_of(recipient)?;

    let mut rng = StdRng::try_from_rng(&mut SysRng).expect("OS entropy source unavailable");
    let mut ephemeral_secret = [0u8; 32];
    rng.fill_bytes(&mut ephemeral_secret);

    let ephemeral_public = MontgomeryPoint::mul_base_clamped(ephemeral_secret);
    let shared = recipient_point.mul_clamped(ephemeral_secret);
    if shared.is_identity() {
        return Err(SealError::InvalidRecipientKey);
    }

    let (key, nonce) = derive(&ephemeral_public, &recipient_point, &shared);
    let ciphertext = ChaCha20Poly1305::new(&key)
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: &ephemeral_public.0,
            },
        )
        .map_err(|_| SealError::Failed)?;

    Ok(SealedBox {
        ephemeral_public: ephemeral_public.0,
        nonce,
        ciphertext,
    })
}

/// Decrypt a box sealed to `recipient`'s public key.
///
/// Returns [`SealError::Failed`] — never partial or unauthenticated
/// output — if the box was addressed to somebody else, or if any part of
/// it was altered in transit.
pub fn open(recipient: &Keypair, sealed: &SealedBox) -> Result<Vec<u8>, SealError> {
    let recipient_point = montgomery_of(&recipient.public_key()).map_err(|_| SealError::Failed)?;
    let ephemeral_public = MontgomeryPoint(sealed.ephemeral_public);

    let shared = ephemeral_public.mul_clamped(recipient.x25519_secret_bytes());
    if shared.is_identity() {
        return Err(SealError::Failed);
    }

    let (key, _) = derive(&ephemeral_public, &recipient_point, &shared);
    ChaCha20Poly1305::new(&key)
        .decrypt(
            Nonce::from_slice(&sealed.nonce),
            Payload {
                msg: &sealed.ciphertext,
                aad: &ephemeral_public.0,
            },
        )
        .map_err(|_| SealError::Failed)
}

/// The recipient's Ed25519 key in its birationally equivalent Montgomery
/// (X25519) form.
fn montgomery_of(public_key: &PublicKey) -> Result<MontgomeryPoint, SealError> {
    VerifyingKey::from_bytes(public_key.as_bytes())
        .map(|key| key.to_montgomery())
        .map_err(|_| SealError::InvalidRecipientKey)
}

/// The AEAD key and nonce for one seal.
///
/// Both transcripts commit to the ephemeral public key, the recipient's
/// public key, and the ECDH output — so a box sealed to one gateway
/// derives a different key under any other gateway's identity, and the
/// (key, nonce) pair is unique per ephemeral key without needing a
/// separate random nonce on the wire.
fn derive(
    ephemeral_public: &MontgomeryPoint,
    recipient: &MontgomeryPoint,
    shared: &MontgomeryPoint,
) -> (Key, [u8; 12]) {
    let transcript = |domain: &[u8]| -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(domain);
        hasher.update(ephemeral_public.0);
        hasher.update(recipient.0);
        hasher.update(shared.0);
        hasher.finalize().into()
    };
    let key = Key::from(transcript(KEY_DOMAIN));
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&transcript(NONCE_DOMAIN)[..12]);
    (key, nonce)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_destination() {
        let gateway = Keypair::generate();
        let sealed = seal(&gateway.public_key(), b"user@example.com").unwrap();
        assert_eq!(open(&gateway, &sealed).unwrap(), b"user@example.com");
    }

    #[test]
    fn round_trips_an_empty_plaintext() {
        let gateway = Keypair::generate();
        let sealed = seal(&gateway.public_key(), b"").unwrap();
        assert_eq!(open(&gateway, &sealed).unwrap(), b"");
    }

    #[test]
    fn never_puts_the_plaintext_on_the_wire() {
        let gateway = Keypair::generate();
        let sealed = seal(&gateway.public_key(), b"+254700000000").unwrap();
        assert!(
            !sealed
                .ciphertext
                .windows(13)
                .any(|window| window == b"+254700000000"),
            "the whole point of a sealed box is that gossip never carries the destination"
        );
    }

    #[test]
    fn two_seals_of_the_same_plaintext_differ() {
        let gateway = Keypair::generate();
        let first = seal(&gateway.public_key(), b"user@example.com").unwrap();
        let second = seal(&gateway.public_key(), b"user@example.com").unwrap();
        assert_ne!(first.ephemeral_public, second.ephemeral_public);
        assert_ne!(first.ciphertext, second.ciphertext);
    }

    #[test]
    fn the_wrong_recipient_cannot_open_it() {
        let gateway = Keypair::generate();
        let eavesdropper = Keypair::generate();
        let sealed = seal(&gateway.public_key(), b"user@example.com").unwrap();
        assert_eq!(open(&eavesdropper, &sealed), Err(SealError::Failed));
    }

    #[test]
    fn a_tampered_ciphertext_fails_instead_of_returning_garbage() {
        let gateway = Keypair::generate();
        let mut sealed = seal(&gateway.public_key(), b"user@example.com").unwrap();
        sealed.ciphertext[0] ^= 0x01;
        assert_eq!(open(&gateway, &sealed), Err(SealError::Failed));
    }

    #[test]
    fn a_tampered_ephemeral_key_fails_instead_of_returning_garbage() {
        let gateway = Keypair::generate();
        let mut sealed = seal(&gateway.public_key(), b"user@example.com").unwrap();
        sealed.ephemeral_public[0] ^= 0x01;
        assert_eq!(open(&gateway, &sealed), Err(SealError::Failed));
    }

    #[test]
    fn a_tampered_nonce_fails_instead_of_returning_garbage() {
        let gateway = Keypair::generate();
        let mut sealed = seal(&gateway.public_key(), b"user@example.com").unwrap();
        sealed.nonce[0] ^= 0x01;
        assert_eq!(open(&gateway, &sealed), Err(SealError::Failed));
    }

    #[test]
    fn a_sealed_box_survives_the_wire_format() {
        let gateway = Keypair::generate();
        let sealed = seal(&gateway.public_key(), b"@handle").unwrap();
        let json = serde_json::to_vec(&sealed).unwrap();
        let decoded: SealedBox = serde_json::from_slice(&json).unwrap();
        assert_eq!(open(&gateway, &decoded).unwrap(), b"@handle");
    }

    #[test]
    fn rejects_a_public_key_that_is_not_a_curve_point() {
        // y = 2^254, which has no square root for x on Ed25519 — so it
        // never decompresses to a point at all.
        let mut bytes = [0u8; 32];
        bytes[31] = 0x7f;
        assert_eq!(
            seal(&PublicKey::from_bytes(bytes), b"user@example.com"),
            Err(SealError::InvalidRecipientKey)
        );
    }

    #[test]
    fn rejects_a_small_order_public_key() {
        // The Ed25519 identity point (y = 1). It decompresses fine, but
        // every ECDH against it yields the identity — a shared "secret"
        // an attacker already knows.
        let mut bytes = [0u8; 32];
        bytes[0] = 1;
        assert_eq!(
            seal(&PublicKey::from_bytes(bytes), b"user@example.com"),
            Err(SealError::InvalidRecipientKey)
        );
    }
}
