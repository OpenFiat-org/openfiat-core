//! `openfiat-registry` — Service discovery/registry for node-provided services.
//!
//! Implements OFS-1500 (SRP) on top of `openfiat_gossip`: registrations,
//! health updates, and withdrawals travel as gossip events (§19 — "the
//! registry is fully decentralized... changes propagate through the
//! Gossip Protocol") rather than a bespoke transport, and every node
//! derives its local registry purely by consuming them.

pub mod error;
pub mod health;
pub mod protocol;
pub mod record;
pub mod registration;
pub mod service;
pub mod store;
pub mod withdrawal;

pub use error::RegistryError;
pub use health::{HealthState, HealthUpdate, SignedHealthUpdate};
pub use record::ServiceRecord;
pub use registration::{Registration, SignedRegistration};
pub use service::RegistryService;
pub use store::Registry;
pub use withdrawal::{SignedWithdrawal, Withdrawal};

use openfiat_serialization::wire;
use openfiat_types::EventEnvelope;

/// A gossip event decoded into whichever registry payload it carries.
pub(crate) enum RegistryEvent {
    Registered(SignedRegistration),
    Updated(SignedHealthUpdate),
    Unregistered(SignedWithdrawal),
}

pub(crate) fn parse_event(event: &EventEnvelope) -> Option<RegistryEvent> {
    match event.event_type.as_str() {
        protocol::EVENT_REGISTERED => wire::from_bytes(&event.payload).ok().map(RegistryEvent::Registered),
        protocol::EVENT_UPDATED => wire::from_bytes(&event.payload).ok().map(RegistryEvent::Updated),
        protocol::EVENT_UNREGISTERED => wire::from_bytes(&event.payload).ok().map(RegistryEvent::Unregistered),
        _ => None,
    }
}

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
