//! Node identity types (OFNP §6-7, ONSP §5).
//!
//! Key *material* (private keys, signing, verification) belongs to
//! `openfiat-crypto`, not here — a "types" crate that pulled in secret-key
//! handling would force every downstream consumer (RPC clients, the
//! explorer, SDKs) to depend on cryptographic code they never touch. This
//! module defines only the public, wire-visible shapes: a public key, the
//! derived peer identifier, and the signature bytes that accompany a
//! signed message.

/// An Ed25519 public key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
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

/// An Ed25519 signature.
///
/// Stored as `Vec<u8>` rather than `[u8; 64]`: `serde`'s derive only
/// implements (de)serialization for fixed-size arrays up to length 32
/// without pulling in an extra big-array dependency, and 64 bytes is
/// outside that range. `from_bytes`/`as_bytes` still take/return the
/// fixed-size array so callers get the same compile-time length guarantee.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

/// A node's network identifier, deterministically derived from its
/// [`PublicKey`] (OFNP §6).
///
/// The derivation itself (libp2p's multihash-wrapped public key encoding)
/// is `openfiat-network`'s responsibility, since it's the only crate that
/// otherwise needs a libp2p dependency. This type just carries the result
/// so every other crate can reference a peer without knowing how the ID
/// was produced.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
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
}
