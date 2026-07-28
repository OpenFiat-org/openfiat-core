//! Identity failures (OFS-5000 §19), mapped onto OFS-8000's Identity
//! range (2000-2999) where a code exists there, and the closest
//! applicable code otherwise.

use openfiat_types::ErrorCode;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityError {
    InvalidSignature,
    Unauthorized,
    DuplicateClaimId,
    MalformedClaim,
    ClaimNotFound,
    /// §19: an action attempted on a claim that no longer accepts it
    /// (e.g. verifying or revoking an already-revoked claim).
    InvalidClaimState,
}

impl IdentityError {
    pub const fn code(self) -> ErrorCode {
        match self {
            Self::InvalidSignature => ErrorCode::InvalidSignature,
            Self::Unauthorized => ErrorCode::InvalidIdentityClaim,
            Self::DuplicateClaimId => ErrorCode::IdentityAlreadyExists,
            Self::MalformedClaim => ErrorCode::DeserializationError,
            Self::ClaimNotFound => ErrorCode::IdentityNotFound,
            Self::InvalidClaimState => ErrorCode::InvalidIdentityClaim,
        }
    }
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code().name())
    }
}

impl std::error::Error for IdentityError {}
