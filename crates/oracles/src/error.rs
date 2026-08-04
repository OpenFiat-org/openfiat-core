//! Oracle failures. OFS-8000 allocates no error range for OFS-7000 at
//! all (its range table stops at Notifications/Internal) — each variant
//! here maps to the closest existing general/network-range code instead
//! of inventing an unregistered one, the same approach `openfiat-registry`
//! takes for OFS-1500.

use openfiat_types::ErrorCode;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OracleError {
    InvalidSignature,
    /// §15: "reject unauthorized providers" — the publisher isn't
    /// registered as a market-data provider in `openfiat-registry`.
    Unauthorized,
    MalformedRecord,
    /// §7's `expires_at` isn't after `published_at` — a self-evidently
    /// invalid record, rejected rather than stored as already-stale.
    AlreadyExpired,
    /// §15: "reject duplicate updates" — a publish whose version isn't
    /// strictly greater than the one already on file for this Oracle ID.
    StaleVersion,
    OracleNotFound,
}

impl OracleError {
    pub const fn code(self) -> ErrorCode {
        match self {
            Self::InvalidSignature => ErrorCode::InvalidSignature,
            Self::Unauthorized => ErrorCode::InvalidRequest,
            Self::MalformedRecord => ErrorCode::DeserializationError,
            // The record's `expires_at` does not follow its own
            // `published_at`: one field contradicts another, which is
            // what `InvalidParameter` is for. It used to be
            // `SessionExpired` (1006) — a code naming a session, in a
            // crate that has none, for a record that has not expired and
            // never could, since it was refused before it was ever
            // stored. Distinct from `StaleVersion` below, which is a
            // well-formed record arriving too late; this one is
            // ill-formed on its face and the publisher can see that
            // without asking anyone.
            Self::AlreadyExpired => ErrorCode::InvalidParameter,
            Self::StaleVersion => ErrorCode::InvalidRequest,
            Self::OracleNotFound => ErrorCode::ResourceNotFound,
        }
    }
}

impl fmt::Display for OracleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code().name())
    }
}

impl std::error::Error for OracleError {}
