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
/// The empty acknowledgement a receiver returns for every
/// [`MESSAGE_TYPE_PUSH`].
///
/// A push carries no reply worth having — the sender does not wait, and
/// nothing in the protocol depends on the answer. It exists because the
/// transport underneath is request-response, where every inbound request
/// occupies a stream slot until it is answered or times out. Dropping the
/// channel without responding leaks that slot for the whole timeout, and a
/// node receiving a burst of pushes runs out: `Dropping inbound stream
/// because we are at capacity`, observed on a live node. Answering
/// immediately releases the slot.
pub const MESSAGE_TYPE_PUSH_ACK: &str = "GossipPushAck";

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
