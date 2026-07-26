//! `openfiat-risk` — plugin architecture and provider SDK for OpenFiat risk
//! intelligence adapters.
//!
//! Related specification: OFS-7100 (OpenFiat Risk Intelligence Protocol).
//!
//! This crate defines the `RiskProvider` interface only. Concrete adapters
//! (e.g. for CipherOwl, Chainalysis, TRM, Elliptic, or a community-run
//! provider) are expected to be implemented externally against this trait —
//! none are implemented here.

/// Crate version, re-exported for diagnostics and `openfiat-node --version`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The subject of a risk assessment (an address, identity claim, etc.).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskSubject {
    pub kind: String,
    pub reference: String,
}

/// The result of a risk assessment.
#[derive(Debug, Clone, PartialEq)]
pub struct RiskAssessment {
    pub subject: RiskSubject,
    pub score: f64,
    pub provider: String,
}

/// Implemented by a risk intelligence provider plugin.
pub trait RiskProvider: Send + Sync {
    fn name(&self) -> &str;
    fn assess(&self, subject: &RiskSubject) -> Result<RiskAssessment, RiskError>;
}

/// Errors a [`RiskProvider`] may return.
#[derive(Debug)]
pub enum RiskError {
    NotImplemented,
    ProviderUnavailable(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_a_version() {
        assert!(!version().is_empty());
    }

    #[test]
    fn subject_equality() {
        let a = RiskSubject {
            kind: "address".into(),
            reference: "abc".into(),
        };
        let b = a.clone();
        assert_eq!(a, b);
    }
}
