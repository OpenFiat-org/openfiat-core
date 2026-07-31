//! The materialized, queryable state of one registered service — what
//! `Registry` actually stores, derived from the registration/health/
//! withdrawal events applied to it.

use crate::branding::ServiceBranding;
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
    /// The region the provider declared, if any. Nothing observes it —
    /// see `Registration::region` and `docs/region-is-declared.md`.
    pub region: Option<String>,
    pub capabilities: Vec<String>,
    /// Name, description, logo and website as the provider declared
    /// them, or `None` when they declared nothing. Self-asserted: a
    /// signature proves the record was not altered, never that the name
    /// is the signer's to use.
    pub branding: Option<ServiceBranding>,
    pub pricing: Option<ServicePricing>,
    /// Base58 Solana address earnings are payable to, as declared on the
    /// registration. Required whenever `pricing` is set.
    pub payout_wallet: Option<String>,
    pub health: HealthState,
    pub registered_at: Timestamp,
    pub last_health_update: Timestamp,
}
