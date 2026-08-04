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
            // The answer every other domain in this workspace gives to
            // "you are not who you would have to be for this to be
            // allowed" — `openfiat_disputes`, `openfiat_reviews`,
            // `openfiat_tradechannel` and `openfiat_registry` all use
            // 2001 for it. This used to be `InvalidProposal` (7004),
            // which is a verdict on the proposal rather than on the
            // signer: an author told 7004 for someone else's withdrawal
            // attempt rewrites a proposal that was never wrong.
            Self::Unauthorized => ErrorCode::InvalidIdentityClaim,
            Self::DuplicateProposalId => ErrorCode::ProposalAlreadyExists,
            Self::MalformedProposal => ErrorCode::DeserializationError,
            Self::ProposalNotFound => ErrorCode::ProposalNotFound,
            Self::VotingClosed => ErrorCode::VotingClosed,
            Self::DuplicateVote => ErrorCode::DuplicateVote,
            // Also 7004 until now, and the same confusion in its second
            // form: "this proposal is not in a state that allows that"
            // is fixed by looking at the proposal's status, never by
            // editing its text.
            Self::InvalidStateTransition => ErrorCode::InvalidProposalState,
        }
    }
}

impl fmt::Display for GovernanceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code().name())
    }
}

impl std::error::Error for GovernanceError {}
