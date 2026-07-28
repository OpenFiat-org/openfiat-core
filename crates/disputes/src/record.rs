//! The dispute shape (OFS-2400 §7-8) and its state machine (§14, §16).
//!
//! This crate implements the off-chain P2P coordination the whitepaper's
//! Chapter 11 describes — case formation, commit-reveal voting, consensus
//! determination. Actual OPEN staking and slashing (Ch.11 §11.6, §11.15-
//! 11.16) are Solana program operations this layer doesn't invoke yet,
//! the same documented deferral `openfiat-settlement` makes for escrow
//! release.

use openfiat_settlement::SettlementId;
use openfiat_types::{PeerId, PublicKey, Timestamp};

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct DisputeId(String);

impl DisputeId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// §14's timeline, collapsed to the states that actually change what
/// actions are legal: evidence submission and investigation don't
/// change this (both remain possible throughout `Open`), so they aren't
/// separately persisted states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DisputeStatus {
    /// Escrow frozen (§6); accepting evidence and arbitrators joining.
    Open,
    /// §14: required arbitrator count reached; no further arbitrators may
    /// join; commit phase is live.
    CaseLocked,
    /// Every required arbitrator has committed; reveal phase is live.
    RevealPhase,
    Resolved,
}

/// §17's resolution outcomes (excluding "Partial Settlement", explicitly
/// marked future in the spec).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Resolution {
    BuyerWins,
    MerchantWins,
    MutualSettlement,
    Invalid,
}

/// An arbitrator's vote (distinct from [`Resolution`]: an individual vote
/// doesn't include `MutualSettlement`, which is a party-agreed outcome
/// bypassing arbitration entirely).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Vote {
    BuyerWins,
    MerchantWins,
    Invalid,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ArbitratorCommitment {
    pub arbitrator: PeerId,
    pub commitment: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ArbitratorReveal {
    pub arbitrator: PeerId,
    pub vote: Vote,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Dispute {
    pub id: DisputeId,
    pub settlement_id: SettlementId,
    pub buyer: PeerId,
    pub buyer_public_key: PublicKey,
    pub seller: PeerId,
    pub seller_public_key: PublicKey,
    pub opener: PeerId,
    pub reason: String,
    pub status: DisputeStatus,
    /// How many arbitrators must join before the case locks (§14, §16 —
    /// "the arbitrator threshold required for a case is determined
    /// internally"; a fixed protocol parameter here rather than a
    /// per-case secret, a documented MVP simplification).
    pub required_arbitrators: u8,
    pub arbitrators: Vec<PeerId>,
    pub arbitrator_keys: Vec<(PeerId, PublicKey)>,
    pub commitments: Vec<ArbitratorCommitment>,
    pub reveals: Vec<ArbitratorReveal>,
    pub resolution: Option<Resolution>,
    pub buyer_agreed_mutual_settlement: bool,
    pub seller_agreed_mutual_settlement: bool,
    pub opened_at: Timestamp,
    pub updated_at: Timestamp,
}

impl Dispute {
    pub fn arbitrator_key(&self, arbitrator: &PeerId) -> Option<&PublicKey> {
        self.arbitrator_keys
            .iter()
            .find(|(id, _)| id == arbitrator)
            .map(|(_, key)| key)
    }
}
