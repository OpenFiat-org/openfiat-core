//! Signed health updates (OFS-1500 §11).
//!
//! Unlike a registration, a health update doesn't carry the provider's
//! public key — it's verified against whatever key the registry already
//! has on file for that Service ID (see [`crate::store::Registry::apply_health_update`]),
//! not a self-asserted key in the update payload.

pub use crate::record::HealthState;
use openfiat_crypto::Keypair;
use openfiat_types::{PeerId, ServiceId, Signature, Timestamp};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HealthUpdate {
    pub service_id: ServiceId,
    pub provider: PeerId,
    pub state: HealthState,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedHealthUpdate {
    pub update: HealthUpdate,
    pub signature: Signature,
}

impl SignedHealthUpdate {
    pub fn sign(update: HealthUpdate, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::wire::to_bytes(&update)
            .expect("HealthUpdate always serializes");
        Self {
            signature: keypair.sign(&bytes),
            update,
        }
    }
}
