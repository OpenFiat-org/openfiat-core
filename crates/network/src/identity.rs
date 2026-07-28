//! Bridges `openfiat_crypto`'s Ed25519 keypair into libp2p's identity types.
//!
//! OFNP §6 specifies one Node Identity — public key, private key, Peer ID —
//! reused for both the Noise transport handshake and higher-level protocol
//! message signing. Rather than maintaining two separate keypairs, this
//! module derives libp2p's identity directly from the same 32-byte Ed25519
//! seed `openfiat_crypto::Keypair` already holds, so a node's transport
//! identity and its OFNP-level signing identity are always the same key.

use libp2p::identity::{Keypair as Libp2pKeypair, PeerId as Libp2pPeerId, ed25519};
use openfiat_crypto::Keypair;
use openfiat_types::PeerId;

/// Derive libp2p's identity keypair from an `openfiat_crypto::Keypair`.
pub fn to_libp2p_keypair(keypair: &Keypair) -> Libp2pKeypair {
    let mut seed = keypair.seed();
    let secret =
        ed25519::SecretKey::try_from_bytes(&mut seed).expect("a 32-byte seed is always a valid Ed25519 secret key");
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
}
