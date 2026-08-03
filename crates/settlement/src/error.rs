//! Settlement failures (OFS-2300 §24), mapped onto OFS-8000's Settlement
//! & Liquidity range (5000-5999) where a code exists there, and the
//! closest applicable code otherwise.

use openfiat_types::ErrorCode;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettlementError {
    InvalidSignature,
    /// §24: an action taken by someone other than the settlement's buyer
    /// or seller, or a transition attempted by the wrong party (e.g. the
    /// buyer trying to approve their own payment).
    Unauthorized,
    DuplicateSettlementId,
    MalformedSettlement,
    SettlementNotFound,
    /// §20: an action that doesn't correspond to a legal transition from
    /// the settlement's current state.
    InvalidStateTransition,
}

impl SettlementError {
    pub const fn code(self) -> ErrorCode {
        match self {
            Self::InvalidSignature => ErrorCode::InvalidSignature,
            Self::Unauthorized => ErrorCode::InvalidIdentityClaim,
            Self::DuplicateSettlementId => ErrorCode::SettlementAlreadyCompleted,
            Self::MalformedSettlement => ErrorCode::DeserializationError,
            // Two conditions, two codes, and they used to share one.
            //
            // Both were `SettlementFailed`, which is retryable — so a
            // client could not tell "this node has never heard of that
            // settlement" from "that transition is illegal from where the
            // settlement is now", and was told to try again at both. The
            // second is permanent by construction: nothing about
            // repeating the call moves a settlement back into a state it
            // has already left.
            //
            // It mattered more once cancellation, rejection and payment
            // reversal became reachable over RPC, because "can I still
            // cancel this?" is the question a client asks speculatively
            // and "no, too late" is the answer it has to be able to
            // act on.
            Self::SettlementNotFound => ErrorCode::SettlementNotFound,
            Self::InvalidStateTransition => ErrorCode::InvalidSettlementState,
        }
    }
}

impl fmt::Display for SettlementError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code().name())
    }
}

impl std::error::Error for SettlementError {}
