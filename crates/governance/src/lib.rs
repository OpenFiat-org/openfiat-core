//! `openfiat-governance` — Protocol governance: proposals, voting, parameter changes.
//!
//! Implements OFS-4000 (OGP) on top of `openfiat_gossip`: proposal
//! creation, votes, withdrawal, and activation travel as signed gossip
//! events, and every node derives its local governance state — including
//! quorum/majority resolution — purely by consuming them, the same
//! replication pattern used throughout this workspace. Real voting-power
//! computation from OPEN token balance/stake is a future integration
//! this layer doesn't have yet — see the `record` module doc.

pub mod error;
pub mod events;
pub mod protocol;
pub mod record;
pub mod service;
pub mod store;

pub use error::GovernanceError;
pub use record::{CastVote, Proposal, ProposalCategory, ProposalId, ProposalStatus, VoteChoice};
pub use service::GovernanceService;
pub use store::GovernanceRegistry;

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
