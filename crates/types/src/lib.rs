//! `openfiat-types` — Shared protocol types: identifiers, addresses, amounts, timestamps.
//!
//! The foundation of the workspace's dependency graph (see
//! `docs/architecture.md`): almost every other crate depends on this one,
//! and this crate depends on nothing but `serde`. Anything that needs a
//! heavier dependency (cryptographic signing, libp2p, storage) belongs in
//! the crate that owns that concern instead.

pub mod amount;
pub mod error;
pub mod event;
pub mod identity;
pub mod priority;
pub mod service;
pub mod timestamp;

pub use amount::Amount;
pub use error::ErrorCode;
pub use event::{EventEnvelope, EventId, EventType, InvalidEventType};
pub use identity::{NodeRole, PeerId, PublicKey, Signature};
pub use priority::Priority;
pub use service::{
    InfrastructureService, MarketDataService, MarketplaceService, NotificationChannel,
    SecurityService, ServiceId, ServiceType,
};
pub use timestamp::Timestamp;

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
