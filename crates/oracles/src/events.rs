//! The signed oracle publication event (OFS-7000 §8: "Unsigned oracle
//! updates MUST be rejected"). Self-consistency verified here (the
//! publisher really is who it claims to be); authorization against
//! `openfiat-registry`'s on-file market-data providers happens at the
//! store layer, the same two-tier split `openfiat-notifications` uses
//! for delivery reports.

use crate::error::OracleError;
use crate::record::{OracleData, OracleId};
use openfiat_crypto::{Keypair, verify};
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_types::{PeerId, PublicKey, Signature, Timestamp};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OraclePublish {
    pub id: OracleId,
    pub provider: PeerId,
    pub provider_public_key: PublicKey,
    pub data: OracleData,
    pub version: u64,
    pub timestamp: Timestamp,
    pub expires_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedOraclePublish {
    pub publish: OraclePublish,
    pub signature: Signature,
}

impl SignedOraclePublish {
    pub fn sign(publish: OraclePublish, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::json::to_bytes(&publish)
            .expect("OraclePublish always serializes");
        Self {
            signature: keypair.sign(&bytes),
            publish,
        }
    }

    pub fn verify(&self) -> Result<(), OracleError> {
        let expected = peer_id_from_public_key(&self.publish.provider_public_key)
            .map_err(|_| OracleError::InvalidSignature)?;
        if expected != self.publish.provider {
            return Err(OracleError::Unauthorized);
        }
        let bytes = openfiat_serialization::json::to_bytes(&self.publish)
            .map_err(|_| OracleError::MalformedRecord)?;
        verify(&self.publish.provider_public_key, &bytes, &self.signature)
            .map_err(|_| OracleError::InvalidSignature)
    }
}
