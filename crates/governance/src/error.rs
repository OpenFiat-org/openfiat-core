//! Governance failures, mapped onto OFS-8000's Governance range
//! (7000-7999) where a code exists there, and the closest applicable
//! code otherwise.

use openfiat_types::ErrorCode;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GovernanceError {
    InvalidSignature,
    /// An action attempted by someone other than the proposal's author
    /// (withdrawing or activating a proposal that isn't theirs).
    Unauthorized,
    DuplicateProposalId,
    MalformedProposal,
    ProposalNotFound,
    /// A vote cast outside `ProposalStatus::Voting`, or after
    /// `voting_closes_at`.
    VotingClosed,
    DuplicateVote,
    /// A withdraw/activate attempted from a status that doesn't allow it.
    InvalidStateTransition,
}

impl GovernanceError {
    pub const fn code(self) -> ErrorCode {
        match self {
            Self::InvalidSignature => ErrorCode::InvalidSignature,
            Self::Unauthorized => ErrorCode::InvalidProposal,
            Self::DuplicateProposalId => ErrorCode::InvalidProposal,
            Self::MalformedProposal => ErrorCode::DeserializationError,
            Self::ProposalNotFound => ErrorCode::ProposalNotFound,
            Self::VotingClosed => ErrorCode::VotingClosed,
            Self::DuplicateVote => ErrorCode::DuplicateVote,
            Self::InvalidStateTransition => ErrorCode::InvalidProposal,
        }
    }
}

impl fmt::Display for GovernanceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code().name())
    }
}

impl std::error::Error for GovernanceError {}
