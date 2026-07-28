//! The local plugin interface for querying an external risk intelligence
//! source (e.g. a real provider's API) — kept from this crate's original
//! scaffolding. No concrete adapters ship here; each is expected to live
//! in its own crate/plugin implementing this trait.

/// The subject of a risk assessment (an address, identity claim, etc.) —
/// more general than `record::RiskRecord`'s `wallet: PeerId`, since a
/// local query might target something not yet resolved to a wallet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskSubject {
    pub kind: String,
    pub reference: String,
}

/// The result of a query against an external provider, before it becomes
/// (or contributes to) a signed, published `RiskRecord`.
#[derive(Debug, Clone, PartialEq)]
pub struct RiskAssessment {
    pub subject: RiskSubject,
    pub score: f64,
    pub provider: String,
}

/// Errors a [`RiskProvider`] may return when querying its external
/// source — distinct from `error::RiskError`, which covers this crate's
/// own gossip/replication layer.
#[derive(Debug)]
pub enum ProviderError {
    NotImplemented,
    ProviderUnavailable(String),
}

/// Implemented by a risk intelligence provider plugin.
pub trait RiskProvider: Send + Sync {
    fn name(&self) -> &str;
    fn assess(&self, subject: &RiskSubject) -> Result<RiskAssessment, ProviderError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subject_equality() {
        let a = RiskSubject { kind: "address".into(), reference: "abc".into() };
        let b = a.clone();
        assert_eq!(a, b);
    }
}
