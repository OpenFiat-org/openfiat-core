//! Wallet/signed-request failures, mapped onto OFS-8000's General
//! Protocol and Network ranges — this crate has no OFS spec of its own,
//! so there's no dedicated error range to draw from.

use openfiat_types::ErrorCode;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalletError {
    MalformedRequest,
    InvalidSignature,
    /// The request's timestamp falls outside the caller's allowed
    /// freshness window (the nonce/timestamp anti-replay pattern the
    /// backend implementation plan's Phase 7 RPC auth model calls for) —
    /// or is in the future, which is treated the same way.
    RequestExpired,
}

impl WalletError {
    pub const fn code(self) -> ErrorCode {
        match self {
            Self::MalformedRequest => ErrorCode::InvalidRequest,
            Self::InvalidSignature => ErrorCode::InvalidSignature,
            Self::RequestExpired => ErrorCode::SessionExpired,
        }
    }
}

impl fmt::Display for WalletError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code().name())
    }
}

impl std::error::Error for WalletError {}
