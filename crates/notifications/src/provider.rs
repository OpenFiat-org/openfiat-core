//! The delivery plugin interface (OFS-6000 §5) and the payload that
//! crosses it. §2 explicitly leaves the message payload schema out of
//! scope; [`NotificationPayload`] is this crate's concrete answer.
//!
//! One concrete implementation ships here — [`crate::gateway::HttpGateway`],
//! which forwards to a registered gateway over HTTP. Last-mile adapters
//! (SMTP, an SMS aggregator, a Telegram bot) live on the *gateway* side
//! of that hop, not in the node.

use crate::error::NotificationError;
use crate::record::{NotificationId, NotificationTrigger};
use openfiat_crypto::SealedBox;
use openfiat_types::{NotificationChannel, PeerId, ServiceId};

/// §19: providers receive only what delivery requires — a destination and
/// rendered content, never the trade details that produced them.
///
/// The destination is a [`SealedBox`], not a string. The node routing
/// this payload has no way to read it: the wallet sealed it to the bound
/// gateway's `provider_public_key`, and it travels end-to-end opaque.
/// That is a deliberate change from the original plaintext
/// `destination: String` — with subscriptions gossiped to every node, a
/// plaintext field would have made every node a holder of every user's
/// contact details, which is precisely what §19 forbids.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NotificationPayload {
    /// Deterministic across every node that observed the same source
    /// event — see [`NotificationId`]. The gateway MUST treat it as a
    /// deduplication key and deliver at most once per id.
    pub notification_id: NotificationId,
    pub trigger: NotificationTrigger,
    pub recipient_wallet: PeerId,
    /// The gateway this payload is addressed to, as registered in
    /// `openfiat-registry`. Present so a gateway can reject a misrouted
    /// payload outright instead of failing to open a box meant for
    /// somebody else.
    pub service_id: ServiceId,
    pub channel: NotificationChannel,
    /// The delivery address, openable only with the gateway's own
    /// private key (`openfiat_crypto::open`).
    pub sealed_destination: SealedBox,
    pub subject: String,
    pub body: String,
}

/// Implemented by whatever performs the delivery hop. §17: providers
/// never create protocol events, they only deliver ones already verified
/// upstream — `send` receives a payload the caller has already derived
/// from a verified gossip event.
///
/// `send` is async and takes the target `endpoint` explicitly: which
/// endpoint to use is a routing decision made against the live registry
/// (see [`crate::routing`]), not a property of the transport.
#[async_trait::async_trait]
pub trait NotificationProvider: Send + Sync {
    /// The channels this provider is willing to carry. An HTTP forwarder
    /// carries all of them, because the channel-specific work happens on
    /// the far side of the hop; a direct SMTP adapter would return only
    /// [`NotificationChannel::Email`].
    fn channels(&self) -> Vec<NotificationChannel>;

    /// Hand `payload` to `endpoint`. Returning `Ok` means the handoff was
    /// accepted — nothing more. Whether the message reached a human is
    /// the gateway's to observe and report (`DeliveryReport`).
    async fn send(
        &self,
        endpoint: &str,
        payload: &NotificationPayload,
    ) -> Result<(), NotificationError>;
}
