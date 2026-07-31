//! The proposal shape (OFS-4000 §7-9) and its collapsed state machine.
//!
//! §9's full lifecycle (Draft → Community Discussion → Formal Submission
//! → Technical Review → Voting → Accepted/Rejected → Implementation →
//! Activation) includes stages that aren't independently observable
//! protocol events in this off-chain P2P layer: discussion and technical
//! review happen off-protocol (a forum, not gossip), so a proposal opens
//! directly for voting at creation rather than passing through a
//! separately persisted `Draft` state — the same aggressive collapsing
//! this workspace applies to reservations' `Validated`/`Accepted` and
//! settlement's `PaymentSubmitted`/`MerchantReviewing`. Likewise,
//! `Accepted` covers everything from §16's approval through §17's
//! implementation; `Activated` is `Accepted` plus §18's "reference
//! implementation shipped, feature live" acknowledgement — the actual
//! scheduled protocol upgrade mechanics are a future integration, the
//! same documented deferral as settlement's on-chain escrow release.

use openfiat_types::{PeerId, PublicKey, Timestamp};

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ProposalId(String);

impl ProposalId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// §8's examples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ProposalCategory {
    Protocol,
    Economics,
    Marketplace,
    Infrastructure,
    Governance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ProposalStatus {
    /// Open for votes — see the type-level doc for why there's no
    /// separate `Draft` state.
    Voting,
    Accepted,
    Rejected,
    /// §21: withdrawn before voting concluded.
    Withdrawn,
    /// §18: an accepted proposal whose reference implementation has
    /// shipped and is live.
    Activated,
}

/// §13's voting options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum VoteChoice {
    Approve,
    Reject,
    Abstain,
}

/// §14: "The exact voting model is defined by the Tokenomics
/// specification... the governance protocol intentionally separates
/// voting mechanics from governance workflow." This crate accepts
/// whatever `weight` the caller supplies rather than computing one
/// itself — deriving real weight from OPEN token balance/stake is a
/// future integration this workspace doesn't have yet, the same
/// deferred-integration pattern as escrow release and stake slashing.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CastVote {
    pub voter: PeerId,
    pub choice: VoteChoice,
    pub weight: u64,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Proposal {
    pub id: ProposalId,
    pub title: String,
    pub summary: String,
    pub category: ProposalCategory,
    pub author: PeerId,
    pub author_public_key: PublicKey,
    pub status: ProposalStatus,
    pub votes: Vec<CastVote>,
    /// The `openfiat-governance` program `Proposal` this claims to be the
    /// off-chain half of — the `u64` that seeds its PDA — or `None` for a
    /// proposal that never goes on chain.
    ///
    /// Fixed at creation, because it travels inside the signed
    /// `ProposalCreate` event and there is no event that amends it. That
    /// is what makes it usable as half of a join key: an on-chain
    /// proposal's proposer cannot induce an off-chain proposal to claim
    /// it after the fact. See [`crate::onchain`] for the other half and
    /// for why one half alone proves nothing.
    pub onchain_proposal_id: Option<u64>,
    pub voting_closes_at: Timestamp,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl Proposal {
    pub fn vote_by(&self, voter: &PeerId) -> Option<&CastVote> {
        self.votes.iter().find(|vote| &vote.voter == voter)
    }
}
