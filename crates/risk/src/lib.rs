//! `openfiat-risk` — plugin architecture and provider SDK for OpenFiat risk
//! intelligence adapters.
//!
//! Implements OFS-7100 (ORIP) on top of `openfiat_gossip` and
//! `openfiat_registry`: providers register exactly the way any other
//! service does (`ServiceType::Security(SecurityService::RiskIntelligenceProvider)`,
//! no separate registration event here), signed risk records travel as
//! gossip events, and every node derives its local intelligence dataset
//! — including wallet-screening aggregation (§11) — purely by consuming
//! them. `provider` defines the local plugin interface an external
//! adapter (e.g. for CipherOwl, Chainalysis, TRM, Elliptic) uses to query
//! its source before publishing a record; none ship here.

pub mod error;
pub mod events;
pub mod protocol;
pub mod provider;
pub mod record;
pub mod service;
pub mod store;

pub use error::RiskError;
pub use provider::{ProviderError, RiskAssessment, RiskProvider, RiskSubject};
pub use record::{
    Confidence, ProviderCategory, RiskOutcome, RiskRecord, RiskRecordId, ScreeningResult, Severity,
};
pub use service::RiskService;
pub use store::RiskIndex;

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
