//! Node identity types (OFNP §6-7, ONSP §5).
//!
//! Key *material* (private keys, signing, verification) belongs to
//! `openfiat-crypto`, not here — a "types" crate that pulled in secret-key
//! handling would force every downstream consumer (RPC clients, the
//! explorer, SDKs) to depend on cryptographic code they never touch. This
//! module defines only the public, wire-visible shapes: a public key, the
//! derived peer identifier, and the signature bytes that accompany a
//! signed message.

use std::fmt;

/// An Ed25519 public key.
///
/// Serializes as base58 in JSON and as its raw bytes on the compact wire —
/// see [`crate::base58`] for why, and for what that means for signatures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PublicKey([u8; 32]);

impl PublicKey {
    /// Wrap raw Ed25519 public key bytes.
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The raw 32-byte encoding.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&bs58::encode(self.0).into_string())
    }
}

impl serde::Serialize for PublicKey {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        crate::base58::serialize("PublicKey", &self.0, serializer)
    }
}

impl<'de> serde::Deserialize<'de> for PublicKey {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        crate::base58::deserialize_array32(
            "PublicKey",
            "a base58 Ed25519 public key (32 bytes)",
            deserializer,
        )
        .map(Self)
    }
}

/// An Ed25519 signature.
///
/// Stored as `Vec<u8>` rather than `[u8; 64]`: `serde`'s derive only
/// implements (de)serialization for fixed-size arrays up to length 32
/// without pulling in an extra big-array dependency, and 64 bytes is
/// outside that range. `from_bytes`/`as_bytes` still take/return the
/// fixed-size array so callers get the same compile-time length guarantee.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature(Vec<u8>);

impl Signature {
    /// Wrap raw Ed25519 signature bytes.
    pub fn from_bytes(bytes: [u8; 64]) -> Self {
        Self(bytes.to_vec())
    }

    /// The raw 64-byte encoding, or `None` if this value didn't come from
    /// [`Signature::from_bytes`] (e.g. it was deserialized from a
    /// malformed/malicious wire message with the wrong length).
    pub fn as_bytes(&self) -> Option<[u8; 64]> {
        self.0.as_slice().try_into().ok()
    }
}

impl fmt::Display for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&bs58::encode(&self.0).into_string())
    }
}

impl serde::Serialize for Signature {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        crate::base58::serialize("Signature", &self.0, serializer)
    }
}

impl<'de> serde::Deserialize<'de> for Signature {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Deliberately not length-checked here: a wrong-length signature
        // must reach `as_bytes` and fail *verification*, which is a
        // signature failure, rather than failing to parse, which a caller
        // could mistake for a malformed request.
        crate::base58::deserialize_vec("Signature", "a base58 Ed25519 signature", deserializer)
            .map(Self)
    }
}

/// A node's network identifier, deterministically derived from its
/// [`PublicKey`] (OFNP §6).
///
/// The derivation itself (libp2p's multihash-wrapped public key encoding)
/// is `openfiat-network`'s responsibility, since it's the only crate that
/// otherwise needs a libp2p dependency. This type just carries the result
/// so every other crate can reference a peer without knowing how the ID
/// was produced.
///
/// In JSON this is the familiar `12D3Koo…` form — base58btc of the same
/// multihash bytes, which is exactly what libp2p's own `Display` produces
/// and what an `--entrypoint` takes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PeerId(Vec<u8>);

impl PeerId {
    /// Wrap an already-derived peer ID (e.g. `libp2p::PeerId::to_bytes()`).
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// The raw encoded bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Display for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&bs58::encode(&self.0).into_string())
    }
}

impl serde::Serialize for PeerId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        crate::base58::serialize("PeerId", &self.0, serializer)
    }
}

impl<'de> serde::Deserialize<'de> for PeerId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        crate::base58::deserialize_vec("PeerId", "a base58 peer id (12D3Koo…)", deserializer)
            .map(Self)
    }
}

/// A logical role a node advertises during peer negotiation (OFNP §7).
///
/// A single node MAY implement multiple roles simultaneously, so this is
/// deliberately a value used in a `Vec<NodeRole>`/`HashSet<NodeRole>`, not
/// a mutually-exclusive discriminant of the node itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum NodeRole {
    FullNode,
    BootstrapNode,
    SnapshotProvider,
    NotificationGateway,
    OracleProvider,
    RiskIntelligenceProvider,
    MerchantGateway,
    PublicApiNode,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_key_round_trips_its_bytes() {
        let bytes = [7u8; 32];
        assert_eq!(PublicKey::from_bytes(bytes).as_bytes(), &bytes);
    }

    #[test]
    fn peer_id_equality_is_byte_equality() {
        assert_eq!(
            PeerId::from_bytes(vec![1, 2, 3]),
            PeerId::from_bytes(vec![1, 2, 3])
        );
        assert_ne!(
            PeerId::from_bytes(vec![1, 2, 3]),
            PeerId::from_bytes(vec![1, 2, 4])
        );
    }

    /// The exact bytes from the `getProviders` response that prompted this
    /// change: a libp2p peer id whose multihash wraps an Ed25519 *public*
    /// key. Rendered as an integer array, it was indistinguishable from a
    /// leaked secret; rendered as base58 it is the `12D3Koo…` an operator
    /// already recognizes and can paste into an `--entrypoint`.
    const LIVE_NODE_PEER_ID: [u8; 38] = [
        0, 36, 8, 1, 18, 32, 138, 172, 246, 48, 208, 101, 155, 70, 162, 159, 216, 168, 140, 93,
        246, 114, 240, 183, 215, 183, 151, 57, 79, 65, 139, 7, 250, 175, 52, 209, 191, 170,
    ];

    #[test]
    fn a_peer_id_serializes_as_the_12d3koo_form_an_operator_can_actually_use() {
        let peer = PeerId::from_bytes(LIVE_NODE_PEER_ID.to_vec());
        assert_eq!(
            serde_json::to_string(&peer).unwrap(),
            "\"12D3KooWK9hQ7TwbfvFiaAxUbRFCkdhS7iEpAJDnewNL1anyREQ1\""
        );
        assert_eq!(
            peer.to_string(),
            "12D3KooWK9hQ7TwbfvFiaAxUbRFCkdhS7iEpAJDnewNL1anyREQ1"
        );
    }

    #[test]
    fn a_public_key_serializes_as_base58_not_as_an_array_of_integers() {
        // The tail of the same peer id: the naked Ed25519 public key that
        // `getProviders` reported beside it. It base58-encodes to the very
        // value that same response published as the node's `payout_wallet`,
        // which is the plainest possible demonstration that this field was
        // never secret — it was already public, twice, in one response.
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&LIVE_NODE_PEER_ID[6..]);
        let json = serde_json::to_string(&PublicKey::from_bytes(bytes)).unwrap();

        assert_eq!(json, "\"ALLENLMtV1zEAHT3xpVryqcbdPCB8c9JhM1Jdbe5XHg5\"");
        assert!(
            !json.starts_with('['),
            "an array of 32 integers is shaped exactly like an Ed25519 private key, \
             so a reader cannot tell from the response which one they were handed"
        );
    }

    #[test]
    fn json_round_trips_through_base58() {
        let key = PublicKey::from_bytes([9u8; 32]);
        let json = serde_json::to_string(&key).unwrap();
        assert_eq!(serde_json::from_str::<PublicKey>(&json).unwrap(), key);

        let peer = PeerId::from_bytes(LIVE_NODE_PEER_ID.to_vec());
        let json = serde_json::to_string(&peer).unwrap();
        assert_eq!(serde_json::from_str::<PeerId>(&json).unwrap(), peer);

        let signature = Signature::from_bytes([3u8; 64]);
        let json = serde_json::to_string(&signature).unwrap();
        assert_eq!(serde_json::from_str::<Signature>(&json).unwrap(), signature);
    }

    #[test]
    fn a_base58_public_key_of_the_wrong_length_is_refused() {
        // 31 bytes of 0x01 — valid base58, wrong size for a key.
        let short = bs58::encode([1u8; 31]).into_string();
        let error = serde_json::from_str::<PublicKey>(&format!("\"{short}\"")).unwrap_err();
        assert!(
            error.to_string().contains("32 bytes"),
            "expected a length complaint naming the expected size, got: {error}"
        );
    }

    #[test]
    fn text_that_is_not_base58_is_refused_rather_than_silently_truncated() {
        // `0`, `O`, `I` and `l` are the four characters base58 omits.
        assert!(serde_json::from_str::<PublicKey>("\"0OIl\"").is_err());
    }

    /// Guards the gossip wire and every row already on disk. These types
    /// cross `postcard`, which is not human-readable, so the base58 branch
    /// must not apply there — and the compact branch must emit exactly what
    /// `#[derive(Serialize)]` emitted before, byte for byte, or every node
    /// stops understanding its own database and its peers' messages.
    #[test]
    fn postcard_encoding_is_byte_for_byte_what_the_derive_produced() {
        // A newtype over `[u8; 32]`: serde treats the array as a tuple, so
        // postcard writes the 32 bytes with no length prefix.
        let key = PublicKey::from_bytes([7u8; 32]);
        assert_eq!(postcard::to_allocvec(&key).unwrap(), vec![7u8; 32]);

        // A newtype over `Vec<u8>`: a varint length, then the bytes.
        let peer = PeerId::from_bytes(vec![1, 2, 3]);
        assert_eq!(postcard::to_allocvec(&peer).unwrap(), vec![3, 1, 2, 3]);

        let signature = Signature::from_bytes([3u8; 64]);
        let mut expected = vec![64];
        expected.extend_from_slice(&[3u8; 64]);
        assert_eq!(postcard::to_allocvec(&signature).unwrap(), expected);

        // And each still decodes from those bytes.
        assert_eq!(postcard::from_bytes::<PublicKey>(&[7u8; 32]).unwrap(), key);
        assert_eq!(postcard::from_bytes::<PeerId>(&[3, 1, 2, 3]).unwrap(), peer);
        assert_eq!(
            postcard::from_bytes::<Signature>(&expected).unwrap(),
            signature
        );
    }

    #[test]
    fn a_public_key_can_now_be_a_json_map_key() {
        // Impossible while the encoding was an array: JSON object keys are
        // strings, so `serde_json` refused the map outright.
        let map = std::collections::HashMap::from([(PublicKey::from_bytes([1u8; 32]), 5u32)]);
        let json = serde_json::to_string(&map).expect("a base58 key is a string, so this is legal");
        assert!(json.contains("\":5"), "expected a string key, got {json}");
    }
}
