//! The signed risk-intelligence publication event (OFS-7100 §19: "Every
//! intelligence record MUST be digitally signed"). Self-consistency
//! verified here (the publisher really is who it claims to be);
//! authorization against `openfiat-registry`'s on-file risk providers
//! happens at the store layer, the same two-tier split
//! `openfiat-oracles` uses for market-data providers.

use crate::error::RiskError;
use crate::record::{Confidence, ProviderCategory, RiskOutcome, RiskRecordId, Severity};
use openfiat_crypto::{Keypair, verify};
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_types::{PeerId, PublicKey, Signature, Timestamp};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RiskPublish {
    pub id: RiskRecordId,
    pub provider: PeerId,
    pub provider_public_key: PublicKey,
    pub wallet: PeerId,
    pub category: ProviderCategory,
    pub outcome: RiskOutcome,
    pub severity: Severity,
    pub confidence: Confidence,
    pub reason: String,
    pub evidence: Vec<String>,
    pub timestamp: Timestamp,
    pub expires_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedRiskPublish {
    pub publish: RiskPublish,
    pub signature: Signature,
}

impl SignedRiskPublish {
    pub fn sign(publish: RiskPublish, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::wire::to_bytes(&publish).expect("RiskPublish always serializes");
        Self { signature: keypair.sign(&bytes), publish }
    }

    pub fn verify(&self) -> Result<(), RiskError> {
        let expected = peer_id_from_public_key(&self.publish.provider_public_key).map_err(|_| RiskError::InvalidSignature)?;
        if expected != self.publish.provider {
            return Err(RiskError::Unauthorized);
        }
        let bytes = openfiat_serialization::wire::to_bytes(&self.publish).map_err(|_| RiskError::MalformedRecord)?;
        verify(&self.publish.provider_public_key, &bytes, &self.signature).map_err(|_| RiskError::InvalidSignature)
    }
}
