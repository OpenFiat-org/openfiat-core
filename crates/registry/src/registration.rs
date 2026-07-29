//! Signed service registration (OFS-1500 §5, §7-8).

use crate::error::RegistryError;
use crate::pricing::ServicePricing;
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
    /// What this service charges, if anything. OFS-1500 §15 keeps pricing
    /// optional — plenty of providers are free — but a declared price has
    /// to be machine-readable to be billable at all (OFS-4100 §9.5).
    pub pricing: Option<ServicePricing>,
    /// Base58 Solana address earnings are payable to.
    ///
    /// Deliberately its own field rather than being derived from
    /// `provider_public_key`. That key is the node's gossip identity: a
    /// hot key living on an internet-facing daemon. Forcing payouts to it
    /// would mean a provider's earnings accrue to the one key they can
    /// least afford to keep online, with no way to receive at a cold
    /// wallet. They are also different things — the payout target is a
    /// Solana wallet, and the token account funds actually land in is an
    /// ATA derived from it and the mint.
    pub payout_wallet: Option<String>,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedRegistration {
    pub registration: Registration,
    pub signature: Signature,
}

impl SignedRegistration {
    pub fn sign(registration: Registration, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::json::to_bytes(&registration)
            .expect("Registration always serializes");
        Self {
            signature: keypair.sign(&bytes),
            registration,
        }
    }

    /// Verify the signature, that the claimed provider Peer ID actually
    /// derives from the claimed public key (§21 — same peer-poisoning
    /// defense used by `openfiat-discovery`'s advertisements), and that a
    /// service declaring a price also says where it is to be paid.
    pub fn verify(&self) -> Result<(), RegistryError> {
        if self.registration.pricing.is_some() && self.registration.payout_wallet.is_none() {
            return Err(RegistryError::PricingWithoutPayoutWallet);
        }
        let expected = peer_id_from_public_key(&self.registration.provider_public_key)
            .map_err(|_| RegistryError::InvalidSignature)?;
        if expected != self.registration.provider {
            return Err(RegistryError::UnauthorizedUpdate);
        }
        let bytes = openfiat_serialization::json::to_bytes(&self.registration)
            .map_err(|_| RegistryError::MalformedRegistration)?;
        verify(
            &self.registration.provider_public_key,
            &bytes,
            &self.signature,
        )
        .map_err(|_| RegistryError::InvalidSignature)
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
            payout_wallet: r.payout_wallet,
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
            payout_wallet: None,
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
    fn a_structured_price_survives_signing_and_verification() {
        use crate::pricing::{BillingUnit, ServicePricing};
        let keypair = Keypair::generate();
        let provider = peer_id_from_public_key(&keypair.public_key()).unwrap();
        let mut reg = registration(&keypair, provider);
        reg.pricing = Some(ServicePricing {
            token_mint: "29w8TroBTYoaqrXBDcpv5L54VZRA8Kf7kU5U1cakvFdj".to_string(),
            amount: openfiat_types::Amount::new(2_500, 6),
            unit: BillingUnit::Request,
        });
        reg.payout_wallet = Some("EA8TyQ58C3eavg3ThRFTMu1KLyV9e1v2oTQubSBQ9s5z".to_string());

        let signed = SignedRegistration::sign(reg, &keypair);
        assert!(signed.verify().is_ok());
        let record = signed.into_record();
        assert_eq!(record.pricing.unwrap().amount.base_units(), 2_500);
        assert!(record.payout_wallet.is_some());
    }

    #[test]
    fn a_price_without_a_payout_wallet_is_rejected() {
        use crate::pricing::{BillingUnit, ServicePricing};
        let keypair = Keypair::generate();
        let provider = peer_id_from_public_key(&keypair.public_key()).unwrap();
        let mut reg = registration(&keypair, provider);
        reg.pricing = Some(ServicePricing {
            token_mint: "MINT".to_string(),
            amount: openfiat_types::Amount::new(1, 6),
            unit: BillingUnit::Month,
        });
        reg.payout_wallet = None;

        let signed = SignedRegistration::sign(reg, &keypair);
        assert_eq!(
            signed.verify(),
            Err(RegistryError::PricingWithoutPayoutWallet)
        );
    }

    #[test]
    fn a_free_service_still_registers() {
        let keypair = Keypair::generate();
        let provider = peer_id_from_public_key(&keypair.public_key()).unwrap();
        let signed = SignedRegistration::sign(registration(&keypair, provider), &keypair);
        assert!(signed.verify().is_ok(), "pricing stays optional per §15");
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
