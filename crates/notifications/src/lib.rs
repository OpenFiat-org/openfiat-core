//! `openfiat-notifications` — plugin architecture and provider SDK for
//! OpenFiat notification gateways.
//!
//! Implements OFS-6000 (ONP) on top of `openfiat_gossip` and
//! `openfiat_registry`: providers register exactly the way any other
//! service does (`ServiceType::Notifications`, no separate registration
//! event here), wallet subscription preferences and delivery reports
//! travel as their own signed gossip events, and every node derives its
//! local notification state purely by consuming them. `provider` defines
//! the local plugin interface concrete channel adapters (Email, SMS,
//! Telegram, Discord, Web Push, Mobile Push, Webhooks) implement — none
//! are implemented in this crate.

pub mod error;
pub mod events;
pub mod protocol;
pub mod provider;
pub mod record;
pub mod service;
pub mod store;

pub use error::NotificationError;
pub use provider::{NotificationPayload, NotificationProvider};
pub use record::{DeliveryReceipt, DeliveryStatus, NotificationCategory, NotificationId, NotificationTrigger, Subscription};
pub use service::NotificationService;
pub use store::NotificationRegistry;

/// Crate version, re-exported for diagnostics and `openfiat-node --version`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_a_version() {
        assert!(!version().is_empty());
    }
}
