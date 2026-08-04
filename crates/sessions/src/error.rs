//! Session failures. OFS-8000 allocates no error range for OFS-1400 at
//! all — each variant here maps to the closest existing general/network-
//! range code instead of inventing an unregistered one, the same
//! approach `openfiat-registry`/`openfiat-oracles`/`openfiat-risk`/
//! `openfiat-snapshot` take for specs OFS-8000 doesn't cover.

use openfiat_types::ErrorCode;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionError {
    InvalidSignature,
    /// A renew/revoke/migrate signed by someone other than the
    /// session's on-file wallet.
    Unauthorized,
    MalformedSession,
    /// §23: "duplicate Session IDs."
    DuplicateSessionId,
    SessionNotFound,
    /// §16: revocation is permanent — acting on an already-revoked
    /// session is rejected, not silently re-applied.
    AlreadyRevoked,
    /// §18: a renewal/migration whose version doesn't move the session
    /// forward.
    StaleVersion,
}

impl SessionError {
    pub const fn code(self) -> ErrorCode {
        match self {
            Self::InvalidSignature => ErrorCode::InvalidSignature,
            Self::Unauthorized => ErrorCode::InvalidRequest,
            Self::MalformedSession => ErrorCode::DeserializationError,
            Self::DuplicateSessionId => ErrorCode::ResourceAlreadyExists,
            Self::SessionNotFound => ErrorCode::ResourceNotFound,
            // 1014, not `SessionExpired` (1006). Revocation and expiry
            // are the two ways a session ends and they are not
            // interchangeable: expiry is the clock running out, and a
            // renew fixes it; revocation is a decision, it is permanent
            // (§16), and a client that responds to it by renewing is
            // asking for the one thing that will never be granted.
            Self::AlreadyRevoked => ErrorCode::SessionRevoked,
            Self::StaleVersion => ErrorCode::InvalidRequest,
        }
    }
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code().name())
    }
}

impl std::error::Error for SessionError {}
