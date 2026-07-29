//! `openfiat-notifications` — plugin architecture and provider SDK for
//! OpenFiat notification gateways.
//!
//! Implements OFS-6000 (ONP) on top of `openfiat_gossip` and
//! `openfiat_registry`: providers register exactly the way any other
//! service does (`ServiceType::Notifications`, no separate registration
//! event here), wallet subscription preferences and delivery reports
//! travel as their own signed gossip events, and every node derives its
//! local notification state purely by consuming them.
//!
//! Delivery is the path from a verified protocol event to a message a
//! human sees, and it is split deliberately:
//!
//! - `record` / `events` — replicated state, including the
//!   destination-bearing subscription. Destinations are sealed
//!   (`openfiat_crypto::seal`) to the bound gateway, never plaintext,
//!   because subscriptions reach every node on the network.
//! - `routing` — which gateways a (trigger, wallet) pair resolves to,
//!   computed purely from replicated state so every node agrees.
//! - `render` — what the message says, kept deliberately contentless
//!   (§19).
//! - `provider` / `gateway` — the delivery hop itself. The node forwards
//!   a sealed payload to a registered gateway over HTTP; the gateway does
//!   the last mile. The node never learns the destination.
//! - `store` — the node's own dispatch record (what it handed over) kept
//!   apart from a gateway's `DeliveryReceipt` (what the gateway claims
//!   happened afterwards).

pub mod error;
pub mod events;
pub mod gateway;
pub mod protocol;
pub mod provider;
pub mod record;
pub mod render;
pub mod routing;
pub mod service;
pub mod store;

pub use error::NotificationError;
pub use gateway::HttpGateway;
pub use provider::{NotificationPayload, NotificationProvider};
pub use record::{
    DeliveryReceipt, DeliveryStatus, DispatchRecord, NotificationCategory, NotificationId,
    NotificationTrigger, Subscription, SubscriptionDestination,
};
pub use routing::{PlannedDelivery, RoutingPlan, SkipReason, SkippedDestination};
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
