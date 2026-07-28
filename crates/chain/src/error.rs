//! Chain-bridge failures (OFS-4300 §10), mapped onto OFS-8000's Network
//! error range (codes 1010-1013, reserved for this specification).

use openfiat_types::ErrorCode;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainError {
    /// No RPC connection and no not-yet-expired blockhash from gossip.
    ChainUnavailable,
    /// The blockhash a caller asked to build against has exceeded
    /// Solana's own validity window.
    BlockhashExpired,
    /// A relay-requested payload did not deserialize as a well-formed
    /// Solana transaction (OFS-4300 §7 — rejected before submission).
    MalformedTransaction,
    /// The underlying RPC submission itself failed (network error,
    /// node-behind, etc. — distinct from the transaction being malformed).
    TransactionSubmissionFailed,
}

impl ChainError {
    pub const fn code(self) -> ErrorCode {
        match self {
            Self::ChainUnavailable => ErrorCode::ChainUnavailable,
            Self::BlockhashExpired => ErrorCode::BlockhashExpired,
            Self::MalformedTransaction => ErrorCode::MalformedTransaction,
            Self::TransactionSubmissionFailed => ErrorCode::TransactionSubmissionFailed,
        }
    }
}

impl fmt::Display for ChainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code().name())
    }
}

impl std::error::Error for ChainError {}
