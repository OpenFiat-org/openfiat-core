//! `openfiat-governance` — on-chain and off-chain governance tooling for the
//! OpenFiat protocol: proposals, voting, and parameter changes.
//!
//! Related specification: OFS-4000 (OpenFiat Governance Protocol).
//!
//! This crate currently defines data shapes and storage interfaces only.
//! Voting logic, quorum rules, and execution of accepted proposals are not
//! implemented yet — see the workspace ROADMAP.md.

/// Crate version, re-exported for diagnostics and `openfiat-node --version`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// A governance proposal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    pub id: u64,
    pub title: String,
    pub summary: String,
    pub status: ProposalStatus,
}

/// Lifecycle status of a [`Proposal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProposalStatus {
    Draft,
    Voting,
    Accepted,
    Rejected,
    Executed,
}

/// A single vote cast on a proposal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vote {
    pub proposal_id: u64,
    pub voter: String,
    pub weight: u64,
    pub in_favor: bool,
}

/// Storage interface for governance state. No implementation is provided —
/// this crate defines the boundary only.
pub trait GovernanceStore: Send + Sync {
    fn get_proposal(&self, id: u64) -> Option<Proposal>;
    fn list_proposals(&self, status: Option<ProposalStatus>) -> Vec<Proposal>;
    fn record_vote(&mut self, vote: Vote) -> Result<(), GovernanceError>;
}

/// Errors surfaced by a [`GovernanceStore`] implementation.
#[derive(Debug)]
pub enum GovernanceError {
    ProposalNotFound(u64),
    VotingClosed(u64),
    Storage(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_a_version() {
        assert!(!version().is_empty());
    }

    #[test]
    fn proposal_can_be_constructed() {
        let p = Proposal {
            id: 1,
            title: "Example".to_string(),
            summary: "An example proposal.".to_string(),
            status: ProposalStatus::Draft,
        };
        assert_eq!(p.status, ProposalStatus::Draft);
    }
}
