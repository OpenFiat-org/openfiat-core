//! Signed Peer Advertisements (OFS-1100 §8).
//!
//! "Advertisements MUST be signed. Unsigned advertisements MUST be
//! rejected." — [`SignedAdvertisement::verify`] is the single gate every
//! advertisement passes through before its contents (or an equivalent
//! [`crate::record::PeerRecord`]) is trusted.

use crate::error::DiscoveryError;
use openfiat_crypto::{Keypair, verify};
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_types::{NodeRole, PeerId, PublicKey, Signature, Timestamp};

/// The unsigned content of a Peer Advertisement (§8).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Advertisement {
    pub peer_id: PeerId,
    pub public_key: PublicKey,
    pub addresses: Vec<String>,
    pub roles: Vec<NodeRole>,
    pub node_version: String,
    pub supported_ofs: Vec<u16>,
    pub timestamp: Timestamp,
}

/// A [`Advertisement`] plus the signature over it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedAdvertisement {
    pub advertisement: Advertisement,
    pub signature: Signature,
}

impl SignedAdvertisement {
    /// Sign an advertisement about oneself with one's own keypair.
    pub fn sign(advertisement: Advertisement, keypair: &Keypair) -> Result<Self, DiscoveryError> {
        let bytes = openfiat_serialization::wire::to_bytes(&advertisement).map_err(|_| DiscoveryError::MalformedAdvertisement)?;
        Ok(Self { signature: keypair.sign(&bytes), advertisement })
    }

    /// Verify the signature, and that the advertisement is internally
    /// self-consistent: the claimed `peer_id` must actually be the one
    /// `public_key` derives to (§10 identity verification, §21/§25 peer
    /// poisoning resistance).
    pub fn verify(&self) -> Result<(), DiscoveryError> {
        let expected_peer_id =
            peer_id_from_public_key(&self.advertisement.public_key).map_err(|_| DiscoveryError::InvalidPublicKey)?;
        if expected_peer_id != self.advertisement.peer_id {
            return Err(DiscoveryError::PeerIdMismatch);
        }

        let bytes = openfiat_serialization::wire::to_bytes(&self.advertisement).map_err(|_| DiscoveryError::MalformedAdvertisement)?;
        verify(&self.advertisement.public_key, &bytes, &self.signature).map_err(|_| DiscoveryError::InvalidSignature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn advertisement(keypair: &Keypair, peer_id: PeerId) -> Advertisement {
        Advertisement {
            peer_id,
            public_key: keypair.public_key(),
            addresses: vec!["/ip4/127.0.0.1/udp/4001/quic-v1".to_string()],
            roles: vec![NodeRole::FullNode],
            node_version: "1.0.0".to_string(),
            supported_ofs: vec![1000, 1100],
            timestamp: Timestamp::now(),
        }
    }

    #[test]
    fn accepts_a_genuinely_self_consistent_signed_advertisement() {
        let keypair = Keypair::generate();
        let peer_id = peer_id_from_public_key(&keypair.public_key()).unwrap();
        let signed = SignedAdvertisement::sign(advertisement(&keypair, peer_id), &keypair).unwrap();
        assert!(signed.verify().is_ok());
    }

    #[test]
    fn rejects_an_advertisement_signed_by_a_different_key_than_it_claims() {
        let signer = Keypair::generate();
        let claimed_identity = Keypair::generate();
        let claimed_peer_id = peer_id_from_public_key(&claimed_identity.public_key()).unwrap();

        // Signed with `signer`, but the payload claims to be `claimed_identity`.
        let mut ad = advertisement(&claimed_identity, claimed_peer_id);
        ad.public_key = claimed_identity.public_key();
        let signed = SignedAdvertisement::sign(ad, &signer).unwrap();

        assert_eq!(signed.verify(), Err(DiscoveryError::InvalidSignature));
    }

    #[test]
    fn rejects_a_peer_id_that_does_not_match_the_public_key() {
        let keypair = Keypair::generate();
        let wrong_peer_id = PeerId::from_bytes(vec![0, 0, 0]);
        let signed = SignedAdvertisement::sign(advertisement(&keypair, wrong_peer_id), &keypair).unwrap();
        assert_eq!(signed.verify(), Err(DiscoveryError::PeerIdMismatch));
    }

    #[test]
    fn rejects_a_tampered_advertisement() {
        let keypair = Keypair::generate();
        let peer_id = peer_id_from_public_key(&keypair.public_key()).unwrap();
        let mut signed = SignedAdvertisement::sign(advertisement(&keypair, peer_id), &keypair).unwrap();
        signed.advertisement.node_version = "9.9.9".to_string();
        assert_eq!(signed.verify(), Err(DiscoveryError::InvalidSignature));
    }
}
