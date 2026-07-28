//! Bridges `openfiat_crypto`'s Ed25519 keypair into libp2p's identity types.
//!
//! OFNP §6 specifies one Node Identity — public key, private key, Peer ID —
//! reused for both the Noise transport handshake and higher-level protocol
//! message signing. Rather than maintaining two separate keypairs, this
//! module derives libp2p's identity directly from the same 32-byte Ed25519
//! seed `openfiat_crypto::Keypair` already holds, so a node's transport
//! identity and its OFNP-level signing identity are always the same key.

use libp2p::identity::{
    DecodingError, Keypair as Libp2pKeypair, PeerId as Libp2pPeerId, PublicKey as Libp2pPublicKey,
    ed25519,
};
use openfiat_crypto::Keypair;
use openfiat_types::{PeerId, PublicKey};

/// Derive libp2p's identity keypair from an `openfiat_crypto::Keypair`.
pub fn to_libp2p_keypair(keypair: &Keypair) -> Libp2pKeypair {
    let mut seed = keypair.seed();
    let secret = ed25519::SecretKey::try_from_bytes(&mut seed)
        .expect("a 32-byte seed is always a valid Ed25519 secret key");
    Libp2pKeypair::from(ed25519::Keypair::from(secret))
}

/// The `openfiat_types::PeerId` deterministically derived from a libp2p
/// identity keypair's public key (OFNP §6), using libp2p's own standard
/// derivation (multihash-wrapped public key encoding).
pub fn peer_id(libp2p_keypair: &Libp2pKeypair) -> PeerId {
    from_libp2p_peer_id(Libp2pPeerId::from(libp2p_keypair.public()))
}

/// Wrap an already-derived libp2p `PeerId` (e.g. from `Swarm::local_peer_id`
/// or a connection event) as the shared `openfiat_types::PeerId`.
pub fn from_libp2p_peer_id(id: Libp2pPeerId) -> PeerId {
    PeerId::from_bytes(id.to_bytes())
}

/// Derive the `PeerId` a bare `openfiat_types::PublicKey` claims, without
/// needing the corresponding private key.
///
/// Used to check a discovered peer's self-consistency (OFS-1100 §10, §25):
/// an advertisement's stated Peer ID must actually be the one its stated
/// public key derives to, or it's rejected as peer poisoning.
pub fn peer_id_from_public_key(public_key: &PublicKey) -> Result<PeerId, DecodingError> {
    let ed25519_key = ed25519::PublicKey::try_from_bytes(public_key.as_bytes())?;
    let libp2p_key = Libp2pPublicKey::from(ed25519_key);
    Ok(from_libp2p_peer_id(Libp2pPeerId::from_public_key(
        &libp2p_key,
    )))
}

/// Recover the public key a freshly-connected peer's `PeerId` embeds,
/// with no wire round-trip needed. Sound specifically because this
/// workspace's node identity is always Ed25519 (OFNP §6): the libp2p
/// peer-id spec mandates the size-inline "identity" multihash (rather
/// than a one-way hash) whenever the protobuf-encoded public key is
/// under 42 bytes, which an Ed25519 key always is — so decoding it back
/// out is a documented guarantee here, not a coincidence to rely on
/// loosely. Used to auto-populate `GossipService`'s `peer_keys` map on
/// connection (see `crates/gossip/src/service.rs::handle`), since two
/// independently-started nodes have no other shared advance knowledge of
/// each other's signing key.
pub fn public_key_from_peer_id(id: Libp2pPeerId) -> Option<PublicKey> {
    let multihash = multihash::Multihash::<64>::from_bytes(&id.to_bytes()).ok()?;
    let libp2p_key = Libp2pPublicKey::try_decode_protobuf(multihash.digest()).ok()?;
    let ed25519_key = libp2p_key.try_into_ed25519().ok()?;
    Some(PublicKey::from_bytes(ed25519_key.to_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_derives_the_same_peer_id() {
        let keypair = Keypair::from_seed([9u8; 32]);
        let a = peer_id(&to_libp2p_keypair(&keypair));
        let b = peer_id(&to_libp2p_keypair(&keypair));
        assert_eq!(a, b);
    }

    #[test]
    fn different_seeds_derive_different_peer_ids() {
        let a = peer_id(&to_libp2p_keypair(&Keypair::from_seed([1u8; 32])));
        let b = peer_id(&to_libp2p_keypair(&Keypair::from_seed([2u8; 32])));
        assert_ne!(a, b);
    }

    #[test]
    fn public_key_from_peer_id_recovers_the_originating_keypairs_public_key() {
        let keypair = Keypair::from_seed([7u8; 32]);
        let libp2p_peer_id = Libp2pPeerId::from(to_libp2p_keypair(&keypair).public());
        let recovered = public_key_from_peer_id(libp2p_peer_id).unwrap();
        assert_eq!(recovered, keypair.public_key());
    }

    #[test]
    fn peer_id_from_public_key_matches_the_keypair_derived_one() {
        let keypair = Keypair::from_seed([3u8; 32]);
        let from_keypair = peer_id(&to_libp2p_keypair(&keypair));
        let from_public_key = peer_id_from_public_key(&keypair.public_key()).unwrap();
        assert_eq!(from_keypair, from_public_key);
    }
}
