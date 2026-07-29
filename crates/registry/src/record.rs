//! The materialized, queryable state of one registered service — what
//! `Registry` actually stores, derived from the registration/health/
//! withdrawal events applied to it.

use crate::pricing::ServicePricing;
use openfiat_types::{PeerId, PublicKey, ServiceId, ServiceType, Timestamp};

/// Service health, per OFS-1500 §11.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum HealthState {
    Online,
    Maintenance,
    Degraded,
    Offline,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServiceRecord {
    pub service_id: ServiceId,
    pub service_type: ServiceType,
    pub provider: PeerId,
    pub provider_public_key: PublicKey,
    pub endpoints: Vec<String>,
    pub supported_ofs: Vec<u16>,
    pub region: Option<String>,
    pub capabilities: Vec<String>,
    pub pricing: Option<ServicePricing>,
    /// Base58 Solana address earnings are payable to, as declared on the
    /// registration. Required whenever `pricing` is set.
    pub payout_wallet: Option<String>,
    pub health: HealthState,
    pub registered_at: Timestamp,
    pub last_health_update: Timestamp,
}
