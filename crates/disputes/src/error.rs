//! Dispute failures (OFS-2400 §24), mapped onto OFS-8000's Dispute range
//! (6000-6999) where a code exists there, and the closest applicable
//! code otherwise.

use openfiat_types::ErrorCode;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisputeError {
    InvalidSignature,
    Unauthorized,
    DuplicateDisputeId,
    MalformedDispute,
    DisputeNotFound,
    SettlementNotFound,
    /// §24: an action that doesn't correspond to a legal transition from
    /// the dispute's current state (e.g. joining after the case locked).
    InvalidStateTransition,
    /// §5: only a party to the underlying settlement may open a dispute
    /// on it, and only one dispute may be open per settlement.
    NotAParty,
    /// §16: a revealed vote whose hash doesn't match the arbitrator's
    /// earlier commitment — discarded, not counted.
    CommitmentMismatch,
    ArbitrationFull,
    NotAnArbitrator,
}

impl DisputeError {
    pub const fn code(self) -> ErrorCode {
        match self {
            Self::InvalidSignature => ErrorCode::InvalidSignature,
            Self::Unauthorized => ErrorCode::InvalidIdentityClaim,
            Self::DuplicateDisputeId => ErrorCode::DisputeAlreadyOpen,
            Self::MalformedDispute => ErrorCode::DeserializationError,
            Self::DisputeNotFound => ErrorCode::DisputeNotFound,
            // One condition, one code, wherever it is raised. Three
            // crates hold a `SettlementNotFound` and they used to answer
            // it two different ways — `SettlementFailed` here and in
            // `openfiat_settlement`, `ResourceNotFound` in
            // `openfiat_tradechannel` — so "does this node have that
            // settlement?" got a different code depending on which method
            // you happened to ask through.
            Self::SettlementNotFound => ErrorCode::SettlementNotFound,
            Self::InvalidStateTransition => ErrorCode::DisputeClosed,
            Self::NotAParty => ErrorCode::InvalidIdentityClaim,
            Self::CommitmentMismatch => ErrorCode::InvalidEvidence,
            Self::ArbitrationFull => ErrorCode::DisputeClosed,
            Self::NotAnArbitrator => ErrorCode::InvalidIdentityClaim,
        }
    }
}

impl fmt::Display for DisputeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code().name())
    }
}

impl std::error::Error for DisputeError {}
