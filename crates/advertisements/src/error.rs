//! Advertisement failures (OFS-2100 §24), mapped onto OFS-8000's
//! Advertisement range (3000-3999) where a code exists there, and the
//! closest applicable code otherwise.

use openfiat_types::ErrorCode;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdvertisementError {
    InvalidSignature,
    /// §24: an update signed by someone other than the advertisement's
    /// original merchant.
    UnauthorizedUpdate,
    DuplicateAdvertisementId,
    MalformedAdvertisement,
    /// §24: liquidity may never go negative.
    NegativeLiquidity,
    AdvertisementNotFound,
    /// §10: a reservation would exceed the advertisement's available
    /// liquidity.
    InsufficientLiquidity,
}

impl AdvertisementError {
    pub const fn code(self) -> ErrorCode {
        match self {
            Self::InvalidSignature => ErrorCode::InvalidSignature,
            Self::UnauthorizedUpdate => ErrorCode::InvalidIdentityClaim,
            Self::DuplicateAdvertisementId => ErrorCode::DuplicateAdvertisement,
            Self::MalformedAdvertisement => ErrorCode::InvalidAdvertisement,
            Self::NegativeLiquidity => ErrorCode::InvalidAdvertisement,
            Self::AdvertisementNotFound => ErrorCode::AdvertisementNotFound,
            Self::InsufficientLiquidity => ErrorCode::InsufficientAvailableLiquidity,
        }
    }
}

impl fmt::Display for AdvertisementError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code().name())
    }
}

impl std::error::Error for AdvertisementError {}
