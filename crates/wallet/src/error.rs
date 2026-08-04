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
            // 1015, not `SessionExpired` (1006). What expired is this
            // one signed request's freshness window; there is no session
            // in this crate to have expired, and a caller that responds
            // to 1006 by re-authenticating has left the timestamp inside
            // the signed bytes exactly as stale as it was.
            Self::RequestExpired => ErrorCode::RequestExpired,
        }
    }
}

impl fmt::Display for WalletError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code().name())
    }
}

impl std::error::Error for WalletError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mapping is a one-line `match` arm and nothing else in this
    /// workspace reaches it — `openfiat-wallet` is depended on by
    /// `openfiat-cli` alone, so the shared guard in
    /// `openfiat-rpc/tests/error_codes.rs` cannot cover this one.
    ///
    /// `SessionExpired` (1006) is what this used to answer with, and a
    /// caller acting on it re-authenticates: the wrong remedy, since
    /// what expired is the timestamp inside the bytes they signed, and
    /// a new session does not change those bytes.
    #[test]
    fn an_expired_request_is_not_an_expired_session() {
        assert_eq!(
            WalletError::RequestExpired.code(),
            ErrorCode::RequestExpired
        );
        assert_eq!(WalletError::RequestExpired.code().code(), 1015);
        assert!(!WalletError::RequestExpired.code().retryable());
    }
}
