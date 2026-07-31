//! The symmetric half of the channel's hybrid encryption: one content
//! key per trade, and the AEAD that every entry's payload is encrypted
//! under.
//!
//! # Why this is not just another sealed box
//!
//! `openfiat_crypto::seal` already does confidentiality-to-a-named-peer,
//! and this crate uses it unchanged — see [`crate::record::KeyGrant`],
//! which is a `SealedBox` and nothing more. What a sealed box cannot do
//! is address a *set of recipients that grows after the ciphertext was
//! written*, and that is exactly what a disputed trade needs: the
//! arbitrators who must read the conversation are not known, and do not
//! exist, at the moment the parties are talking.
//!
//! So the standard answer applies: encrypt the content once under a fresh
//! random key, and distribute that key by sealing it — 32 bytes per
//! recipient — to each reader as they become known. Widening the audience
//! costs one small grant, not a re-encryption of the history.
//!
//! That is also the *integrity* argument, and it is the stronger one. The
//! alternative — re-sealing each message to an arbitrator when a dispute
//! opens — puts the disclosing party in the position of re-encrypting the
//! transcript they are a party to. Nothing would stop them handing the
//! arbitrator a different conversation than the one the network carried,
//! because the arbitrator would only ever see bytes that party produced
//! after the argument started. Here the arbitrator opens the *original*
//! ciphertexts: signed by their authors, timestamped, gossiped, and
//! already replicated to every node before anyone knew there would be a
//! dispute. The arbitrator reads the history rather than a retelling of
//! it.
//!
//! # What is bound into every payload
//!
//! One key covers the whole trade, so the AEAD's associated data has to
//! carry what the key no longer distinguishes: which settlement, which
//! author, which sequence number, and which kind of entry. Without that
//! binding a party could lift their counterparty's ciphertext and re-post
//! it under their own name — the signature would be their own and would
//! verify — turning "the seller sent me this account number" into
//! something the buyer appears to have said. With it, a payload moved to
//! any other slot fails authentication instead of decrypting.
//!
//! # Padding
//!
//! Every node on the network stores these ciphertexts forever and can
//! measure them. Length alone distinguishes "yes" from a bank account
//! number from a paragraph of argument, so plaintexts are padded to a
//! multiple of [`PADDING_BLOCK`] before encryption. This does not hide
//! the *existence* or timing of an entry — nothing can, in a replicated
//! log — but it removes the cheapest content inference from it.

use crate::error::TradeChannelError;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use openfiat_crypto::sha256;
use openfiat_settlement::SettlementId;
use openfiat_types::PeerId;
use rand::rngs::{StdRng, SysRng};
use rand::{Rng, SeedableRng};

/// Domain separator for [`ChannelKey::id`], so a key id can never be
/// confused with any other digest this workspace derives.
const KEY_ID_DOMAIN: &[u8] = b"openfiat/tradechannel/keyid/v1";
/// Domain separator for the AEAD's associated data.
const BINDING_DOMAIN: &[u8] = b"openfiat/tradechannel/binding/v1";

/// Plaintexts are padded up to a multiple of this before encryption. 256
/// bytes is a compromise: small enough that a one-word reply does not
/// cost a kilobyte on every node's disk forever, large enough that the
/// short entries which leak the most (a "yes", a six-digit reference, a
/// phone number) all land in the same bucket.
pub const PADDING_BLOCK: usize = 256;

/// The largest plaintext this format will encrypt.
///
/// Payment details are a few lines and a chat message is a paragraph;
/// anything approaching this is a file, and files belong in
/// `openfiat-content` behind a CID rather than inline in an event every
/// node stores forever.
pub const MAX_ENTRY_PLAINTEXT: usize = 4096;

/// The largest ciphertext [`crate::TradeChannelRegistry`] will accept.
///
/// Enforced on the ciphertext rather than the plaintext because that is
/// the only one a node can see. Derived from [`MAX_ENTRY_PLAINTEXT`] plus
/// the length prefix, rounded up to a whole [`PADDING_BLOCK`], plus the
/// Poly1305 tag — `the_largest_legal_plaintext_produces_an_acceptable_ciphertext`
/// pins the arithmetic so the two constants cannot drift apart.
pub const MAX_ENTRY_CIPHERTEXT: usize = 4352 + 16;

/// One trade's content-encryption key: 32 random bytes, generated
/// client-side, never derived from anything a node knows.
///
/// Deliberately not `Clone`-cheap-and-forgettable: it is moved and
/// borrowed explicitly so a key cannot end up copied into a log line by
/// accident. [`Self::expose`] is the one way to get the bytes out, and it
/// is named to be conspicuous at the call site that seals it.
#[derive(Clone, PartialEq, Eq)]
pub struct ChannelKey([u8; 32]);

impl ChannelKey {
    /// A fresh key for a new channel.
    ///
    /// # Panics
    /// Panics if the operating system's entropy source is unavailable —
    /// the same stance `openfiat_crypto::seal` takes, since a channel key
    /// drawn from predictable randomness is worse than no channel.
    pub fn generate() -> Self {
        let mut rng = StdRng::try_from_rng(&mut SysRng).expect("OS entropy source unavailable");
        let mut bytes = [0u8; 32];
        rng.fill_bytes(&mut bytes);
        Self(bytes)
    }

    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The raw key, for the one operation that legitimately needs it:
    /// sealing it to a recipient (see [`crate::record::KeyGrant`]).
    pub const fn expose(&self) -> &[u8; 32] {
        &self.0
    }

    /// A short, public, non-reversible label for this key.
    ///
    /// Carried on every grant and every entry so a client can tell which
    /// grant opens which payload. That matters because two clients can
    /// legitimately race to establish a channel — each generating a key
    /// and granting it — and without a label the loser's messages would
    /// simply fail to decrypt with no way to say why. Eight bytes of
    /// SHA-256 over a 32-byte secret: enough to distinguish the handful
    /// of keys one trade could ever have, far too little to attack.
    pub fn id(&self) -> ChannelKeyId {
        let mut transcript = Vec::with_capacity(KEY_ID_DOMAIN.len() + 32);
        transcript.extend_from_slice(KEY_ID_DOMAIN);
        transcript.extend_from_slice(&self.0);
        let digest = sha256(&transcript);
        let mut id = [0u8; 8];
        id.copy_from_slice(&digest[..8]);
        ChannelKeyId(id)
    }
}

/// Deliberately opaque: a `ChannelKey` must never reach a log, a panic
/// message, or a test failure diff.
impl std::fmt::Debug for ChannelKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ChannelKey({:?})", self.id())
    }
}

/// The public label of a [`ChannelKey`]. Safe to gossip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ChannelKeyId([u8; 8]);

impl ChannelKeyId {
    pub const fn from_bytes(bytes: [u8; 8]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 8] {
        &self.0
    }
}

/// What an entry's payload is cryptographically nailed to. Every field
/// here is already public in the entry record; binding them means a
/// ciphertext cannot be moved to a different slot without failing to
/// open.
pub struct EntryBinding<'a> {
    pub settlement_id: &'a SettlementId,
    pub author: &'a PeerId,
    pub sequence: u64,
    /// The entry kind's wire name — see [`crate::record::EntryKind::name`].
    pub kind: &'static str,
}

impl EntryBinding<'_> {
    /// The associated-data transcript. Every field is length-prefixed, so
    /// no two distinct bindings can concatenate to the same bytes.
    fn transcript(&self) -> Vec<u8> {
        let sequence = self.sequence.to_be_bytes();
        let mut transcript = Vec::new();
        for field in [
            BINDING_DOMAIN,
            self.settlement_id.as_str().as_bytes(),
            self.author.as_bytes(),
            sequence.as_slice(),
            self.kind.as_bytes(),
        ] {
            transcript.extend_from_slice(&(field.len() as u32).to_le_bytes());
            transcript.extend_from_slice(field);
        }
        transcript
    }
}

/// One entry's encrypted payload, exactly as it travels over gossip and
/// sits on every node's disk.
///
/// `key_id` is public on purpose (see [`ChannelKey::id`]). `nonce` is
/// random per entry rather than derived, because one key encrypts many
/// entries written by two independent clients that cannot coordinate a
/// counter.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChannelCiphertext {
    pub key_id: ChannelKeyId,
    pub nonce: [u8; 12],
    pub ciphertext: Vec<u8>,
}

/// Encrypt `plaintext` for one specific slot in one specific channel.
///
/// Client-side only. A node never calls this — it holds no channel key
/// and has nothing to encrypt — but it lives here rather than in the SDKs
/// alone so the format has one executable definition, and so this crate's
/// own tests can prove what a third party can and cannot read.
pub fn seal_entry(
    key: &ChannelKey,
    binding: &EntryBinding<'_>,
    plaintext: &[u8],
) -> Result<ChannelCiphertext, TradeChannelError> {
    if plaintext.len() > MAX_ENTRY_PLAINTEXT {
        return Err(TradeChannelError::EntryTooLarge);
    }
    let mut rng = StdRng::try_from_rng(&mut SysRng).expect("OS entropy source unavailable");
    let mut nonce = [0u8; 12];
    rng.fill_bytes(&mut nonce);

    let ciphertext = ChaCha20Poly1305::new(Key::from_slice(key.expose()))
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &pad(plaintext),
                aad: &binding.transcript(),
            },
        )
        .map_err(|_| TradeChannelError::PayloadDidNotOpen)?;

    Ok(ChannelCiphertext {
        key_id: key.id(),
        nonce,
        ciphertext,
    })
}

/// Decrypt a payload written for this exact slot.
///
/// Returns [`TradeChannelError::PayloadDidNotOpen`] — never partial or
/// unauthenticated output — for a wrong key, a tampered ciphertext, or a
/// payload lifted from a different settlement, author, sequence or kind.
pub fn open_entry(
    key: &ChannelKey,
    binding: &EntryBinding<'_>,
    payload: &ChannelCiphertext,
) -> Result<Vec<u8>, TradeChannelError> {
    let padded = ChaCha20Poly1305::new(Key::from_slice(key.expose()))
        .decrypt(
            Nonce::from_slice(&payload.nonce),
            Payload {
                msg: &payload.ciphertext,
                aad: &binding.transcript(),
            },
        )
        .map_err(|_| TradeChannelError::PayloadDidNotOpen)?;
    unpad(&padded)
}

/// `u32` little-endian length, the plaintext, then zeros to the next
/// [`PADDING_BLOCK`] boundary. The length prefix is inside the
/// ciphertext, so an observer sees only the padded size.
fn pad(plaintext: &[u8]) -> Vec<u8> {
    let unpadded = 4 + plaintext.len();
    let padded = unpadded.div_ceil(PADDING_BLOCK) * PADDING_BLOCK;
    let mut buffer = Vec::with_capacity(padded);
    buffer.extend_from_slice(&(plaintext.len() as u32).to_le_bytes());
    buffer.extend_from_slice(plaintext);
    buffer.resize(padded, 0);
    buffer
}

/// The inverse, refusing anything whose declared length does not fit the
/// buffer it arrived in. The AEAD already authenticated these bytes, so a
/// bad length here means a bug rather than an attack — but a bug that
/// panicked on a gossiped payload would be a remote crash.
fn unpad(padded: &[u8]) -> Result<Vec<u8>, TradeChannelError> {
    if padded.len() < 4 {
        return Err(TradeChannelError::PayloadDidNotOpen);
    }
    let declared = u32::from_le_bytes([padded[0], padded[1], padded[2], padded[3]]) as usize;
    padded
        .get(4..4 + declared)
        .map(<[u8]>::to_vec)
        .ok_or(TradeChannelError::PayloadDidNotOpen)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::EntryKind;

    /// One slot in one channel, owned by the test so a borrowed
    /// [`EntryBinding`] can point at it. The binding tests below each vary
    /// exactly one of its four fields and assert the payload stops
    /// opening, which is what "bound to its slot" means.
    struct Slot {
        settlement_id: SettlementId,
        author: PeerId,
    }

    impl Slot {
        fn new(settlement: &str, author: &str) -> Self {
            Self {
                settlement_id: SettlementId::new(settlement),
                author: PeerId::from_bytes(author.as_bytes().to_vec()),
            }
        }

        fn binding(&self) -> EntryBinding<'_> {
            self.binding_at(0, EntryKind::PaymentDetails)
        }

        fn binding_at(&self, sequence: u64, kind: EntryKind) -> EntryBinding<'_> {
            EntryBinding {
                settlement_id: &self.settlement_id,
                author: &self.author,
                sequence,
                kind: kind.name(),
            }
        }
    }

    fn slot() -> Slot {
        Slot::new("settle-1", "seller")
    }

    #[test]
    fn a_payload_round_trips_under_the_key_it_was_written_for() {
        let key = ChannelKey::generate();
        let slot = slot();
        let binding = slot.binding();
        let sealed = seal_entry(&key, &binding, b"Equity Bank 0110123456789").unwrap();
        assert_eq!(
            open_entry(&key, &binding, &sealed).unwrap(),
            b"Equity Bank 0110123456789"
        );
    }

    #[test]
    fn an_empty_payload_round_trips() {
        let key = ChannelKey::generate();
        let slot = slot();
        let binding = slot.binding();
        let sealed = seal_entry(&key, &binding, b"").unwrap();
        assert_eq!(open_entry(&key, &binding, &sealed).unwrap(), b"");
    }

    /// The whole reason this crate exists: the account number must not be
    /// recoverable from the bytes every node on the network stores.
    #[test]
    fn the_account_number_never_appears_in_the_ciphertext() {
        let key = ChannelKey::generate();
        let slot = slot();
        let binding = slot.binding();
        let sealed = seal_entry(&key, &binding, b"0110123456789").unwrap();
        assert!(
            !sealed
                .ciphertext
                .windows(13)
                .any(|window| window == b"0110123456789"),
            "gossip carries this blob to every node; the account number \
             must not be in it"
        );
    }

    #[test]
    fn another_key_cannot_open_it() {
        let slot = slot();
        let binding = slot.binding();
        let sealed = seal_entry(&ChannelKey::generate(), &binding, b"secret").unwrap();
        assert_eq!(
            open_entry(&ChannelKey::generate(), &binding, &sealed),
            Err(TradeChannelError::PayloadDidNotOpen)
        );
    }

    /// The binding's job. Without it the buyer could re-post the seller's
    /// ciphertext under their own name and their own signature, and the
    /// record would read as though the buyer said it.
    #[test]
    fn a_payload_moved_to_another_authors_slot_does_not_open() {
        let key = ChannelKey::generate();
        let sellers = Slot::new("settle-1", "seller");
        let buyers = Slot::new("settle-1", "buyer");

        let sealed = seal_entry(&key, &sellers.binding(), b"pay to account 0110123456789").unwrap();
        assert_eq!(
            open_entry(&key, &buyers.binding(), &sealed),
            Err(TradeChannelError::PayloadDidNotOpen)
        );
    }

    #[test]
    fn a_payload_moved_to_another_settlement_does_not_open() {
        let key = ChannelKey::generate();
        let here = Slot::new("settle-1", "seller");
        let there = Slot::new("settle-2", "seller");

        let sealed = seal_entry(&key, &here.binding(), b"details").unwrap();
        assert_eq!(
            open_entry(&key, &there.binding(), &sealed),
            Err(TradeChannelError::PayloadDidNotOpen)
        );
    }

    #[test]
    fn a_payload_moved_to_another_sequence_number_does_not_open() {
        let key = ChannelKey::generate();
        let slot = slot();

        let sealed = seal_entry(
            &key,
            &slot.binding_at(0, EntryKind::PaymentDetails),
            b"details",
        )
        .unwrap();
        assert_eq!(
            open_entry(
                &key,
                &slot.binding_at(1, EntryKind::PaymentDetails),
                &sealed
            ),
            Err(TradeChannelError::PayloadDidNotOpen)
        );
    }

    /// Relabelling payment details as chat (or the reverse) would let a
    /// party change what a client renders an entry as without touching a
    /// byte of the ciphertext.
    #[test]
    fn a_payload_relabelled_as_another_kind_does_not_open() {
        let key = ChannelKey::generate();
        let slot = slot();

        let sealed = seal_entry(
            &key,
            &slot.binding_at(0, EntryKind::PaymentDetails),
            b"details",
        )
        .unwrap();
        assert_eq!(
            open_entry(&key, &slot.binding_at(0, EntryKind::Message), &sealed),
            Err(TradeChannelError::PayloadDidNotOpen)
        );
    }

    #[test]
    fn a_tampered_ciphertext_fails_instead_of_returning_garbage() {
        let key = ChannelKey::generate();
        let slot = slot();
        let binding = slot.binding();
        let mut sealed = seal_entry(&key, &binding, b"details").unwrap();
        sealed.ciphertext[0] ^= 0x01;
        assert_eq!(
            open_entry(&key, &binding, &sealed),
            Err(TradeChannelError::PayloadDidNotOpen)
        );
    }

    /// Length is the cheapest inference available to every node that
    /// stores these forever, so "yes" and a bank account number must not
    /// be distinguishable by size.
    #[test]
    fn short_entries_of_different_lengths_produce_identical_ciphertext_sizes() {
        let key = ChannelKey::generate();
        let slot = slot();
        let binding = slot.binding();
        let tiny = seal_entry(&key, &binding, b"ok").unwrap();
        let longer = seal_entry(&key, &binding, b"Equity Bank 0110123456789, R. Kimani").unwrap();
        assert_eq!(tiny.ciphertext.len(), longer.ciphertext.len());
        assert_eq!(tiny.ciphertext.len(), PADDING_BLOCK + 16);
    }

    #[test]
    fn a_plaintext_over_the_limit_is_refused_rather_than_truncated() {
        let key = ChannelKey::generate();
        let slot = slot();
        let binding = slot.binding();
        let oversized = vec![b'x'; MAX_ENTRY_PLAINTEXT + 1];
        assert_eq!(
            seal_entry(&key, &binding, &oversized),
            Err(TradeChannelError::EntryTooLarge)
        );
    }

    /// `MAX_ENTRY_CIPHERTEXT` is what the registry enforces, and it is
    /// derived by hand from `MAX_ENTRY_PLAINTEXT` and `PADDING_BLOCK`.
    /// If any of the three moves without the others, a legal entry starts
    /// being rejected on every node at once — so pin the arithmetic here.
    #[test]
    fn the_largest_legal_plaintext_produces_an_acceptable_ciphertext() {
        let key = ChannelKey::generate();
        let slot = slot();
        let binding = slot.binding();
        let largest = vec![b'x'; MAX_ENTRY_PLAINTEXT];
        let sealed = seal_entry(&key, &binding, &largest).unwrap();
        assert_eq!(sealed.ciphertext.len(), MAX_ENTRY_CIPHERTEXT);
        assert_eq!(open_entry(&key, &binding, &sealed).unwrap(), largest);
    }

    #[test]
    fn two_keys_have_different_ids_and_one_key_has_a_stable_id() {
        let key = ChannelKey::generate();
        assert_eq!(key.id(), key.id());
        assert_ne!(key.id(), ChannelKey::generate().id());
    }

    /// A key must not be reconstructible from anything that gets written
    /// down, and `Debug` output is the classic accidental leak.
    #[test]
    fn debug_output_reveals_the_key_id_and_not_the_key() {
        let key = ChannelKey::from_bytes([7u8; 32]);
        let rendered = format!("{key:?}");
        assert!(!rendered.contains("7, 7, 7"), "{rendered}");
        assert!(rendered.contains("ChannelKeyId"), "{rendered}");
    }
}
