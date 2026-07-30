//! `openfiat-advertisements` — Advertisement publication and matching.
//!
//! Implements OFS-2100 (OAP) on top of `openfiat_gossip`: advertisement
//! lifecycle events (created/disabled/pricing-updated) travel as gossip
//! events, and every node derives its local advertisement index purely by
//! consuming them (§23) — the same replication pattern `openfiat-registry`
//! uses.
//!
//! Floating pricing resolves in [`pricing`], as a pure function of an
//! oracle read the caller supplies — this crate still depends on no
//! oracle crate, and still stores no resolved price on the replicated
//! record. Merchant-tier capacity limits and risk-weighted visibility
//! remain configuration shapes carried without being resolved.

pub mod error;
pub mod events;
pub mod pricing;
pub mod protocol;
pub mod query;
pub mod record;
pub mod service;
pub mod store;

pub use error::AdvertisementError;
pub use pricing::{MidPrice, PriceQuote, UnpriceableReason};
pub use query::{AdvertisementFilter, AdvertisementPage, DEFAULT_PAGE, MAX_PAGE, Page};
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
