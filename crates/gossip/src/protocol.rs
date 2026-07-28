//! Wire message shapes, carried as `openfiat_network::Envelope` payloads —
//! same rationale as `openfiat-discovery`'s peer exchange: OFNP §20 wants
//! one multiplexed connection carrying every protocol, not a new libp2p
//! protocol per OFS spec.

use crate::channel::Subscription;
use openfiat_types::EventEnvelope;

pub const OFS_SPEC: u16 = 1200;

/// An event being pushed to a peer — forwarding (§13) and origination
/// broadcast both use this.
pub const MESSAGE_TYPE_PUSH: &str = "GossipPush";

/// "Nodes recovering after downtime SHALL request missing events" (§22),
/// also sent on every fresh connection to converge after a partition (§17).
pub const MESSAGE_TYPE_RECOVERY_REQUEST: &str = "GossipRecoveryRequest";
pub const MESSAGE_TYPE_RECOVERY_RESPONSE: &str = "GossipRecoveryResponse";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RecoveryRequest {
    pub subscription: Subscription,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RecoveryResponse {
    pub events: Vec<EventEnvelope>,
}
