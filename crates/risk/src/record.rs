//! The risk record shape (OFS-7100 §6-9, §12).

use openfiat_types::{PeerId, PublicKey, Timestamp};

/// §6's provider-category examples — what kind of intelligence source
/// produced a record, distinct from the record's own severity (§8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ProviderCategory {
    BlockchainAnalytics,
    FraudIntelligence,
    Compliance,
    CommunityIntelligence,
    InfrastructureIntelligence,
}

/// §8's tiers, declared ascending so `Ord`/`max` treat `Critical` as the
/// most severe — the order that actually matters for aggregation (§11,
/// §13), not the descending order the spec lists them in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub enum Severity {
    /// Watchlist, ongoing investigation, monitoring only.
    Informational,
    /// Newly created wallet, limited trading history, elevated but
    /// unconfirmed risk.
    Low,
    /// Suspicious behavior, repeated disputes, high-risk transaction
    /// patterns.
    Medium,
    /// Known scam wallet, confirmed phishing, fraud ring, money mule.
    High,
    /// Stolen funds, sanctions, terrorist financing, ransomware.
    Critical,
}

/// §9's confidence levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Confidence {
    Unknown,
    Low,
    Medium,
    High,
    VeryHigh,
}

/// §14: "Updated intelligence creates new signed records. Historical
/// records remain available for audit purposes" — a wallet is cleared by
/// publishing a new `Cleared` record, not by mutating or deleting the
/// earlier `Flagged` one. See `store::RiskIndex::screen` for how the two
/// interact during aggregation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RiskOutcome {
    Flagged,
    Cleared,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct RiskRecordId(String);

impl RiskRecordId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// §7's field list. Records are immutable and permanent once published
/// (§14) — there is no update/delete operation, only new records.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RiskRecord {
    pub id: RiskRecordId,
    pub provider: PeerId,
    pub provider_public_key: PublicKey,
    pub wallet: PeerId,
    pub category: ProviderCategory,
    pub outcome: RiskOutcome,
    pub severity: Severity,
    pub confidence: Confidence,
    pub reason: String,
    /// §10: evidence references — transaction hashes, report URLs,
    /// case numbers — not the evidence itself.
    pub evidence: Vec<String>,
    pub published_at: Timestamp,
    pub expires_at: Option<Timestamp>,
}

impl RiskRecord {
    /// §12: "Expired data SHOULD NOT be treated as current."
    pub fn is_current(&self, now: Timestamp) -> bool {
        self.expires_at.is_none_or(|expiry| now.as_millis() < expiry.as_millis())
    }
}

/// §11's "Results Aggregated" step, computed by [`crate::store::RiskIndex::screen`].
#[derive(Debug, Clone, PartialEq)]
pub struct ScreeningResult {
    pub wallet: PeerId,
    /// `None` means no current, unsuperseded flag exists — either no
    /// provider has ever reported on this wallet, or the most recent
    /// applicable record is a `Cleared` one.
    pub highest_severity: Option<Severity>,
    /// The current, unsuperseded `Flagged` records contributing to
    /// `highest_severity` — empty when the wallet is clear.
    pub active_flags: Vec<RiskRecord>,
}
