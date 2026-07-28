//! Signed service registration (OFS-1500 §5, §7-8).

use crate::error::RegistryError;
use crate::record::{HealthState, ServiceRecord};
use openfiat_crypto::{Keypair, verify};
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_types::{PeerId, PublicKey, ServiceId, ServiceType, Signature, Timestamp};

/// The unsigned content of a service registration (§7's field list, minus
/// pricing being explicitly optional per §15).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Registration {
    pub service_id: ServiceId,
    pub service_type: ServiceType,
    pub provider: PeerId,
    pub provider_public_key: PublicKey,
    pub endpoints: Vec<String>,
    pub supported_ofs: Vec<u16>,
    pub region: Option<String>,
    pub capabilities: Vec<String>,
    pub pricing: Option<String>,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedRegistration {
    pub registration: Registration,
    pub signature: Signature,
}

impl SignedRegistration {
    pub fn sign(registration: Registration, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::wire::to_bytes(&registration).expect("Registration always serializes");
        Self { signature: keypair.sign(&bytes), registration }
    }

    /// Verify the signature and that the claimed provider Peer ID actually
    /// derives from the claimed public key (§21 — same peer-poisoning
    /// defense used by `openfiat-discovery`'s advertisements).
    pub fn verify(&self) -> Result<(), RegistryError> {
        let expected =
            peer_id_from_public_key(&self.registration.provider_public_key).map_err(|_| RegistryError::InvalidSignature)?;
        if expected != self.registration.provider {
            return Err(RegistryError::UnauthorizedUpdate);
        }
        let bytes = openfiat_serialization::wire::to_bytes(&self.registration).map_err(|_| RegistryError::MalformedRegistration)?;
        verify(&self.registration.provider_public_key, &bytes, &self.signature).map_err(|_| RegistryError::InvalidSignature)
    }

    pub fn into_record(self) -> ServiceRecord {
        let r = self.registration;
        ServiceRecord {
            service_id: r.service_id,
            service_type: r.service_type,
            provider: r.provider,
            provider_public_key: r.provider_public_key,
            endpoints: r.endpoints,
            supported_ofs: r.supported_ofs,
            region: r.region,
            capabilities: r.capabilities,
            pricing: r.pricing,
            health: HealthState::Online,
            registered_at: r.timestamp,
            last_health_update: r.timestamp,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registration(keypair: &Keypair, provider: PeerId) -> Registration {
        Registration {
            service_id: ServiceId::new("svc-1"),
            service_type: ServiceType::MarketData(openfiat_types::MarketDataService::FxOracle),
            provider,
            provider_public_key: keypair.public_key(),
            endpoints: vec!["/ip4/127.0.0.1/udp/4001/quic-v1".to_string()],
            supported_ofs: vec![1500, 7000],
            region: Some("Kenya".to_string()),
            capabilities: vec!["KES/USD".to_string()],
            pricing: None,
            timestamp: Timestamp::now(),
        }
    }

    #[test]
    fn accepts_a_genuinely_self_consistent_signed_registration() {
        let keypair = Keypair::generate();
        let provider = peer_id_from_public_key(&keypair.public_key()).unwrap();
        let signed = SignedRegistration::sign(registration(&keypair, provider), &keypair);
        assert!(signed.verify().is_ok());
    }

    #[test]
    fn rejects_a_provider_id_that_does_not_match_the_public_key() {
        let keypair = Keypair::generate();
        let wrong_provider = PeerId::from_bytes(vec![0, 0, 0]);
        let signed = SignedRegistration::sign(registration(&keypair, wrong_provider), &keypair);
        assert_eq!(signed.verify(), Err(RegistryError::UnauthorizedUpdate));
    }

    #[test]
    fn rejects_a_tampered_registration() {
        let keypair = Keypair::generate();
        let provider = peer_id_from_public_key(&keypair.public_key()).unwrap();
        let mut signed = SignedRegistration::sign(registration(&keypair, provider), &keypair);
        signed.registration.region = Some("Uganda".to_string());
        assert_eq!(signed.verify(), Err(RegistryError::InvalidSignature));
    }
}
