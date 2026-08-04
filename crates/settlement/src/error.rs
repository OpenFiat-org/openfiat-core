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
    /// §24's "duplicate settlement": an initiate whose id this node
    /// already holds. Says nothing about the state — or the parties — of
    /// the settlement that holds it.
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
            // 5010, not `SettlementAlreadyCompleted` (5005), which is
            // what this answered with until now and is a claim about the
            // trade rather than about the id. A client re-sending an
            // initiate after a dropped connection — the ordinary way to
            // reach this — was told its trade had completed, when the
            // settlement holding that id may be sitting at
            // `AwaitingPayment` waiting for that same client's payment,
            // or may belong to two other parties. Believing 5005 means
            // hanging up on a payment still owed.
            Self::DuplicateSettlementId => ErrorCode::SettlementAlreadyExists,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A duplicate id and a completed trade are different statements, and
    /// this crate made them the same one on the wire.
    ///
    /// Worth a test rather than a comment because the failure is silent:
    /// both codes are non-retryable, both are in the 5000 range, and a
    /// client acting on either stops re-sending. The difference is what
    /// it does next — a client told `SETTLEMENT_ALREADY_COMPLETED` has
    /// been told its trade is finished, and stops waiting for a payment
    /// the settlement may still be sitting at `AwaitingPayment` for.
    #[test]
    fn a_duplicate_id_is_not_a_completed_trade() {
        assert_eq!(
            SettlementError::DuplicateSettlementId.code(),
            ErrorCode::SettlementAlreadyExists
        );
        assert_ne!(
            SettlementError::DuplicateSettlementId.code(),
            ErrorCode::SettlementAlreadyCompleted
        );
        assert_eq!(SettlementError::DuplicateSettlementId.code().code(), 5010);
    }

    /// Nothing about re-sending the same initiate frees an id that is
    /// already taken.
    #[test]
    fn a_duplicate_id_is_not_retryable() {
        assert!(!SettlementError::DuplicateSettlementId.code().retryable());
    }
}
