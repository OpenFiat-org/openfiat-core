//! The encryption key a wallet publishes so other people can seal things
//! to it.
//!
//! # The problem this solves
//!
//! [`crate::seal`] addresses a recipient's Ed25519 key and converts it to
//! its Montgomery form. Opening such a box needs the recipient's secret
//! scalar. A node has one. A gateway has one. **A person using a browser
//! wallet does not**: Solana wallets expose `signMessage` and
//! `signTransaction` and no key material at all, by design and rightly.
//!
//! So every mechanism in this workspace that seals to a *wallet* — the
//! `KeyGrant` that opens a confidential trade channel, above all — sealed
//! to a key its recipient could never use. Two ordinary users could not
//! exchange payment details. The feature existed and was unusable.
//!
//! # The fix: a second key, derived from the first, published as a claim
//!
//! A wallet derives an X25519 keypair from its own signature over
//! [`DERIVATION_MESSAGE`] — a fixed, domain-separated string — and
//! publishes the public half as an OFS-5000
//! `ClaimType::EncryptionKey` claim. Grants are sealed to *that*. The
//! secret never leaves the client and is never stored: it is re-derived on
//! demand from a signature the wallet will make on any device.
//!
//! Three properties fall out of the shape, and all three are the reason
//! for it:
//!
//! - **Recoverable.** The same wallet on a new machine re-derives the same
//!   key and opens every grant ever addressed to it. A randomly generated
//!   client key would strand a user on one browser profile, which is the
//!   state this replaced.
//! - **Bound to the wallet.** The claim carries `wallet_public_key` and is
//!   signed by it, and `SignedClaimPublish::verify` refuses a claim whose
//!   `wallet` does not derive from that key. Nobody can publish an
//!   encryption key on somebody else's behalf.
//! - **Not held by anyone else.** No node, no gateway and no arbitrator
//!   ever sees the secret. The alternative that keeps coming up — have the
//!   node issue an identity key — is key escrow with better manners: it
//!   would make every node operator a reader of every trade, which is the
//!   precise thing `openfiat-tradechannel` exists to prevent.
//!
//! # What this is not, stated plainly
//!
//! **The signature is the private key.** Anything that obtains a wallet's
//! signature over [`DERIVATION_MESSAGE`] can derive the secret and read
//! every channel that wallet is party to, past and future — there is no
//! forward secrecy here and there deliberately cannot be (see
//! `docs/trade-channel.md`). A phishing site that persuades a user to sign
//! this exact string has taken their trade history, permanently. It has
//! *not* taken their funds: this is an off-chain message, it authorises no
//! transfer, and it is not a valid serialized Solana transaction message
//! either (see `the_message_cannot_be_read_as_a_solana_transaction`).
//!
//! The defences are what a message can offer and no more: it names
//! OpenFiat in its first line, says in the prompt what signing it does,
//! and is worded so that a wallet's signature dialog reads as a warning
//! rather than as a formality. That is weaker than "cannot be phished" and
//! is not dressed up as more.
//!
//! **Determinism is load-bearing.** Ed25519 signing is deterministic by
//! construction (RFC 8032 §5.1.6 derives the per-signature nonce by
//! hashing the private prefix with the message, using no randomness), and
//! this crate's own signer is checked for it in
//! `the_derivation_is_stable_across_signatures`. A wallet that
//! nevertheless randomised its signatures — a randomised-nonce or
//! hedged-signature implementation — would derive a different key every
//! time and its user would lose access to their own channels. That failure
//! must never be silent, so a client is required to *check* rather than
//! assume: sign twice at enrolment and compare, and on every later use
//! compare the derived public key against the published claim. See
//! `openfiat-app`'s `lib/channel-identity.ts`, which does both.
//!
//! **Losing the wallet loses the channels.** The key is a function of the
//! wallet's secret and nothing else, so it survives a lost device and does
//! not survive a lost seed phrase. There is no recovery path and none is
//! implied. The same follows for a user who signs with a *different*
//! wallet implementation over the same seed: if that implementation wraps
//! the bytes before signing rather than signing them raw, its signature —
//! and so its derived key — differs, and the mismatch shows up as a failed
//! comparison against the published claim rather than as silently
//! unreadable messages.

use crate::seal::{SealError, SealedBox, open_x25519, seal_to_x25519};
use curve25519_dalek::montgomery::MontgomeryPoint;
use curve25519_dalek::traits::IsIdentity;
use openfiat_types::{ErrorCode, Signature};
use sha2::{Digest, Sha512};
use std::fmt;

/// The exact bytes a wallet signs to derive its encryption key.
///
/// **This string is a wire format.** Changing so much as a space changes
/// every key derived from it, which would orphan every channel on the
/// network. A new version gets a new constant and a new claim, not an edit
/// here.
///
/// It is domain-separated by its first line, which no other signed payload
/// in this protocol can produce: domain events are signed over JSON (they
/// begin `{`), and the gated-read handshake signs
/// `<domain>:<subject>:<nonce>` for a fixed set of domains, none of which
/// is this. It is also prose rather than structured data on purpose — a
/// wallet renders it verbatim in the approval dialog, and the person
/// reading it is the last line of defence.
pub const DERIVATION_MESSAGE: &str = "OpenFiat encryption key (v1)\n\
\n\
Signing this message derives the private key that decrypts your OpenFiat trade \
messages and payment details. It is not a transaction: it cannot move funds and \
it sends nothing anywhere.\n\
\n\
Only sign it on a site you trust. Anyone who obtains this signature can read \
every trade conversation and every payment detail this wallet is party to, \
forever.";

/// Domain separator for turning a signature into a secret scalar. Distinct
/// from the sealed-box separators so the two derivations can never collide.
const SEED_DOMAIN: &[u8] = b"openfiat/encryptionkey/v1/x25519";

/// A wallet's published X25519 encryption key: 32 bytes, base58 in a claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EncryptionPublicKey([u8; 32]);

/// Why a published encryption key was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncryptionKeyError {
    /// Not base58, or not 32 bytes once decoded.
    Malformed,
    /// A small-order point. Every ECDH against one yields a shared
    /// "secret" that is public knowledge, so a grant sealed to it would be
    /// readable by anybody — refused at parse time rather than at seal
    /// time, so it can never reach a claim the network replicates.
    SmallOrder,
    /// The signature the key was to be derived from is not 64 bytes.
    MalformedSignature,
}

impl EncryptionKeyError {
    /// The OFS-8000 code this failure maps to.
    pub const fn code(self) -> ErrorCode {
        match self {
            Self::Malformed | Self::SmallOrder => ErrorCode::InvalidParameter,
            Self::MalformedSignature => ErrorCode::InvalidSignature,
        }
    }
}

impl fmt::Display for EncryptionKeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed => write!(f, "not a base58 X25519 public key of 32 bytes"),
            Self::SmallOrder => write!(f, "X25519 public key is small-order and seals to nobody"),
            Self::MalformedSignature => write!(f, "derivation signature is not 64 bytes"),
        }
    }
}

impl std::error::Error for EncryptionKeyError {}

impl EncryptionPublicKey {
    /// Wrap raw X25519 public key bytes, rejecting a small-order point.
    pub fn from_bytes(bytes: [u8; 32]) -> Result<Self, EncryptionKeyError> {
        // Clamped scalars are multiples of the cofactor, so multiplying by
        // one annihilates any point in the small-order subgroup and leaves
        // every other point alone. That makes this the same test the seal
        // itself applies, run early enough to keep a useless key out of
        // the replicated claim store.
        if MontgomeryPoint(bytes).mul_clamped([1u8; 32]).is_identity() {
            return Err(EncryptionKeyError::SmallOrder);
        }
        Ok(Self(bytes))
    }

    /// The raw 32-byte encoding.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Parse the base58 spelling a `ClaimType::EncryptionKey` claim carries.
    pub fn parse(text: &str) -> Result<Self, EncryptionKeyError> {
        let bytes: [u8; 32] = bs58::decode(text)
            .into_vec()
            .map_err(|_| EncryptionKeyError::Malformed)?
            .try_into()
            .map_err(|_| EncryptionKeyError::Malformed)?;
        Self::from_bytes(bytes)
    }

    /// Seal `plaintext` so only the holder of this key's secret can read it.
    pub fn seal(&self, plaintext: &[u8]) -> Result<SealedBox, SealError> {
        seal_to_x25519(&self.0, plaintext)
    }
}

impl fmt::Display for EncryptionPublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&bs58::encode(self.0).into_string())
    }
}

/// A wallet's encryption keypair, derived rather than generated.
///
/// The secret is deliberately not readable from outside this type: a
/// caller opens boxes through [`EncryptionKeypair::open`] and never holds
/// the scalar, the same contract [`crate::Keypair`] keeps for its Ed25519
/// seed. There is nothing to persist — re-deriving costs one wallet
/// signature — so there is no `secret_bytes` to be tempted into storing.
pub struct EncryptionKeypair {
    secret: [u8; 32],
    public: EncryptionPublicKey,
}

impl EncryptionKeypair {
    /// Derive from a wallet's Ed25519 signature over [`DERIVATION_MESSAGE`].
    ///
    /// The caller is responsible for having signed *that* message: this
    /// function cannot check what a signature is over, and deriving from a
    /// signature over anything else silently produces a key nobody can
    /// address.
    pub fn from_wallet_signature(signature: &Signature) -> Result<Self, EncryptionKeyError> {
        let bytes = signature
            .as_bytes()
            .ok_or(EncryptionKeyError::MalformedSignature)?;
        Ok(Self::from_signature_bytes(&bytes))
    }

    /// The same derivation from the raw 64 signature bytes, for callers
    /// holding a wallet's output rather than an [`Signature`].
    pub fn from_signature_bytes(signature: &[u8; 64]) -> Self {
        let mut hasher = Sha512::new();
        hasher.update(SEED_DOMAIN);
        hasher.update(signature);
        let digest = hasher.finalize();
        let mut secret = [0u8; 32];
        secret.copy_from_slice(&digest[..32]);
        Self::from_secret(secret)
    }

    /// Build from a raw X25519 scalar. Only for tests and for a client
    /// that has just derived one; there is no production path that stores
    /// these bytes.
    pub fn from_secret(secret: [u8; 32]) -> Self {
        // `mul_base_clamped` clamps, and so does the ECDH in `open_x25519`,
        // so the caller never has to know which form these bytes are in.
        let public = MontgomeryPoint::mul_base_clamped(secret).0;
        Self {
            secret,
            public: EncryptionPublicKey(public),
        }
    }

    /// The half that goes in the claim.
    pub const fn public_key(&self) -> EncryptionPublicKey {
        self.public
    }

    /// Open a box sealed to [`EncryptionKeypair::public_key`].
    pub fn open(&self, sealed: &SealedBox) -> Result<Vec<u8>, SealError> {
        open_x25519(&self.secret, sealed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keypair::Keypair;

    fn derived(seed: [u8; 32]) -> EncryptionKeypair {
        let wallet = Keypair::from_seed(seed);
        EncryptionKeypair::from_wallet_signature(&wallet.sign(DERIVATION_MESSAGE.as_bytes()))
            .unwrap()
    }

    #[test]
    fn a_grant_sealed_to_a_published_key_opens_under_the_derived_secret() {
        let alice = derived([1u8; 32]);
        let sealed = alice.public_key().seal(b"channel-key-bytes").unwrap();
        assert_eq!(alice.open(&sealed).unwrap(), b"channel-key-bytes");
    }

    #[test]
    fn the_other_party_opens_nothing() {
        let alice = derived([1u8; 32]);
        let bob = derived([2u8; 32]);
        let to_alice = alice.public_key().seal(b"channel-key-bytes").unwrap();
        assert_eq!(bob.open(&to_alice), Err(SealError::Failed));
    }

    /// The property the whole design rests on. Ed25519 is deterministic by
    /// construction, and this is the assertion that says so out loud for
    /// the signer this workspace ships.
    #[test]
    fn the_derivation_is_stable_across_signatures() {
        let wallet = Keypair::from_seed([5u8; 32]);
        let first = wallet.sign(DERIVATION_MESSAGE.as_bytes());
        let second = wallet.sign(DERIVATION_MESSAGE.as_bytes());
        assert_eq!(first, second, "Ed25519 signing must be deterministic");
        assert_eq!(
            EncryptionKeypair::from_wallet_signature(&first)
                .unwrap()
                .public_key(),
            EncryptionKeypair::from_wallet_signature(&second)
                .unwrap()
                .public_key(),
        );
    }

    #[test]
    fn a_different_wallet_derives_a_different_key() {
        assert_ne!(
            derived([1u8; 32]).public_key(),
            derived([2u8; 32]).public_key()
        );
    }

    #[test]
    fn a_signature_over_anything_else_derives_a_different_key() {
        let wallet = Keypair::from_seed([5u8; 32]);
        let right =
            EncryptionKeypair::from_wallet_signature(&wallet.sign(DERIVATION_MESSAGE.as_bytes()))
                .unwrap();
        let wrong =
            EncryptionKeypair::from_wallet_signature(&wallet.sign(b"OpenFiat encryption key (v1)"))
                .unwrap();
        assert_ne!(right.public_key(), wrong.public_key());
    }

    #[test]
    fn a_published_key_survives_the_claim_encoding() {
        let alice = derived([1u8; 32]);
        let text = alice.public_key().to_string();
        assert_eq!(
            EncryptionPublicKey::parse(&text).unwrap(),
            alice.public_key()
        );
    }

    #[test]
    fn refuses_a_claim_value_that_is_not_a_key() {
        assert_eq!(
            EncryptionPublicKey::parse("not base58 at all!"),
            Err(EncryptionKeyError::Malformed)
        );
        // 31 bytes: decodes, wrong length.
        assert_eq!(
            EncryptionPublicKey::parse(&bs58::encode([1u8; 31]).into_string()),
            Err(EncryptionKeyError::Malformed)
        );
    }

    #[test]
    fn refuses_a_small_order_claim_value() {
        // The X25519 identity, and the order-2 point. A grant sealed to
        // either is readable by anybody who can do arithmetic.
        for point in [[0u8; 32], {
            let mut p = [0u8; 32];
            p[0] = 1;
            p
        }] {
            assert_eq!(
                EncryptionPublicKey::parse(&bs58::encode(point).into_string()),
                Err(EncryptionKeyError::SmallOrder),
                "small-order point must never reach a claim"
            );
        }
    }

    /// A `Signature` carries whatever length it was given so that a
    /// malformed one fails verification rather than failing to parse —
    /// which means one can reach this derivation, and must be refused
    /// rather than hashed into a key nobody else will ever compute.
    #[test]
    fn refuses_a_signature_that_is_not_sixty_four_bytes() {
        let short: Signature =
            serde_json::from_value(serde_json::json!(bs58::encode([0u8; 63]).into_string()))
                .unwrap();
        assert_eq!(
            EncryptionKeypair::from_wallet_signature(&short).err(),
            Some(EncryptionKeyError::MalformedSignature)
        );
    }

    /// The derivation message must not be signable *as a transaction* by
    /// accident. A Solana message begins with the number of required
    /// signatures, and every one of those needs a 32-byte account key, so
    /// a buffer this short can only parse if its first byte is tiny. Ours
    /// is `O` (79), demanding 2528 bytes of keys that are not there.
    #[test]
    fn the_message_cannot_be_read_as_a_solana_transaction() {
        let bytes = DERIVATION_MESSAGE.as_bytes();
        let required_signatures = bytes[0] as usize;
        assert!(
            bytes.len() < required_signatures * 32,
            "the message is long enough to parse as a transaction message header"
        );
    }

    /// It must also be unmistakable for anything else this protocol asks a
    /// wallet to sign, or a signature collected for one purpose would open
    /// the other.
    #[test]
    fn the_message_collides_with_no_other_signed_payload() {
        assert!(
            !DERIVATION_MESSAGE.starts_with('{'),
            "domain events are signed over JSON"
        );
        for domain in [
            "openfiat-my-trade-channel",
            "openfiat-my-settlements",
            "openfiat-counterparties",
        ] {
            assert!(
                !DERIVATION_MESSAGE.starts_with(domain),
                "gated reads sign <domain>:<subject>:<nonce>"
            );
        }
    }
}
