//! `openfiat-notifications` — plugin architecture and provider SDK for
//! OpenFiat notification gateways.
//!
//! Related specification: OFS-6000 (OpenFiat Notification Protocol).
//!
//! This crate defines the `NotificationProvider` interface only. Concrete
//! channel adapters (Email, SMS, Telegram, Discord, Signal, Slack, Push,
//! Webhooks, Matrix) are expected to be implemented externally against this
//! trait — none are implemented here.

/// Crate version, re-exported for diagnostics and `openfiat-node --version`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// A notification to be delivered to a recipient.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationMessage {
    pub recipient: String,
    pub subject: String,
    pub body: String,
}

/// Implemented by a notification channel provider plugin.
pub trait NotificationProvider: Send + Sync {
    /// Channel identifier, e.g. `"email"`, `"telegram"`, `"webhook"`.
    fn channel(&self) -> &str;
    fn send(&self, message: &NotificationMessage) -> Result<(), NotificationError>;
}

/// Errors a [`NotificationProvider`] may return.
#[derive(Debug)]
pub enum NotificationError {
    NotImplemented,
    DeliveryFailed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_a_version() {
        assert!(!version().is_empty());
    }

    #[test]
    fn message_can_be_constructed() {
        let m = NotificationMessage {
            recipient: "user@example.com".into(),
            subject: "Trade update".into(),
            body: "Your trade has a new status.".into(),
        };
        assert_eq!(m.recipient, "user@example.com");
    }
}
