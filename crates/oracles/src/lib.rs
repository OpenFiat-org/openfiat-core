//! `openfiat-oracles` — plugin architecture and provider SDK for OpenFiat
//! data oracles (exchange rates, stablecoin metadata, payment networks,
//! country metadata, and future providers such as weather).
//!
//! Implements OFS-7000 (OOP) on top of `openfiat_gossip` and
//! `openfiat_registry`: providers register exactly the way any other
//! service does (`ServiceType::MarketData`, no separate registration
//! event here), signed publications travel as gossip events, and every
//! node derives its local oracle dataset — including median aggregation
//! (§11) — purely by consuming them. `provider` defines the local plugin
//! interface an external provider implementation uses to fetch data
//! before publishing it; no concrete providers ship here.

pub mod error;
pub mod events;
pub mod protocol;
pub mod provider;
pub mod record;
pub mod service;
pub mod store;

pub use error::OracleError;
pub use provider::{ExchangeRate, OracleProvider, OracleRegistry, ProviderError};
pub use record::{OracleCategory, OracleData, OracleId, OracleRecord};
pub use service::OracleService;
pub use store::OracleIndex;

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
