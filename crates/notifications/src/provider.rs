//! The local delivery plugin interface (OFS-6000 §5). Concrete channel
//! adapters (Email, SMS, Telegram, Discord, Web Push, Mobile Push,
//! Webhooks) are expected to be implemented externally against this
//! trait — none are implemented here. §2 explicitly leaves the message
//! payload schema out of scope; `NotificationPayload` is this crate's
//! concrete answer.

use crate::error::NotificationError;
use crate::record::{NotificationId, NotificationTrigger};
use openfiat_types::{NotificationChannel, PeerId};

/// §19: providers should receive only what delivery requires — a
/// destination address and rendered content, not the trade details that
/// produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationPayload {
    pub notification_id: NotificationId,
    pub trigger: NotificationTrigger,
    pub recipient_wallet: PeerId,
    /// The provider-specific delivery address (an email, a phone
    /// number, a webhook URL, a Telegram chat ID, ...).
    pub destination: String,
    pub subject: String,
    pub body: String,
}

/// Implemented by a notification channel provider plugin. §17: providers
/// never create protocol events, they only deliver ones already
/// verified upstream — `send` receives a payload the caller has already
/// derived from a verified gossip event.
pub trait NotificationProvider: Send + Sync {
    fn channel(&self) -> NotificationChannel;
    fn send(&self, payload: &NotificationPayload) -> Result<(), NotificationError>;
}
