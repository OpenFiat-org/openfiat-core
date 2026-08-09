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
    /// §13 anti-spam: the publishing wallet already holds
    /// `store::MAX_CLAIMS_PER_WALLET` live claims and this publish is not a
    /// genuine SUPERSEDE of one of them, so it would add rather than
    /// replace one.
    TooManyClaims,
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
            // Not `RateLimitExceeded`: a rate limit is a speed, and every
            // client that handles one handles it by waiting and retrying.
            // `MAX_CLAIMS_PER_WALLET` is a count that does not decay on its
            // own — nothing frees a slot but revoking, expiry, a genuine
            // SUPERSEDE, or a prune sweep reclaiming a dead claim — so a
            // caller told to back off would back off forever. Same
            // reasoning `openfiat_taxonomy::TaxonomyError::TooManyMethods`
            // already applied to `PaymentMethodLimitReached`.
            Self::TooManyClaims => ErrorCode::ClaimLimitReached,
        }
    }
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code().name())
    }
}

impl std::error::Error for IdentityError {}
