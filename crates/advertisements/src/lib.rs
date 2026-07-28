//! `openfiat-advertisements` — Advertisement publication and matching.
//!
//! Implements OFS-2100 (OAP) on top of `openfiat_gossip`: advertisement
//! lifecycle events (created/disabled/pricing-updated) travel as gossip
//! events, and every node derives its local advertisement index purely by
//! consuming them (§23) — the same replication pattern `openfiat-registry`
//! uses. Floating pricing, merchant-tier capacity limits, and
//! risk-weighted visibility depend on crates that don't exist yet
//! (oracles, reputation, risk — later phases); this crate carries their
//! configuration shapes without resolving them.

pub mod error;
pub mod events;
pub mod protocol;
pub mod record;
pub mod service;
pub mod store;

pub use error::AdvertisementError;
pub use record::{Advertisement, AdvertisementId, AdvertisementStatus, Direction, PricingModel};
pub use service::AdvertisementService;
pub use store::AdvertisementRegistry;

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
