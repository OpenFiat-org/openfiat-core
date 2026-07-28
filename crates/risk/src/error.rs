//! Risk intelligence failures. OFS-8000 allocates no error range for
//! OFS-7100 at all — each variant here maps to the closest existing
//! general/network-range code instead of inventing an unregistered one,
//! the same approach `openfiat-registry` and `openfiat-oracles` take.

use openfiat_types::ErrorCode;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskError {
    InvalidSignature,
    /// §19: "unauthorized provider registration" — the publisher isn't
    /// registered as a risk intelligence provider in `openfiat-registry`.
    Unauthorized,
    MalformedRecord,
    /// §19: "duplicate reports" — a Risk Record ID that's already on file.
    DuplicateRecordId,
}

impl RiskError {
    pub const fn code(self) -> ErrorCode {
        match self {
            Self::InvalidSignature => ErrorCode::InvalidSignature,
            Self::Unauthorized => ErrorCode::InvalidRequest,
            Self::MalformedRecord => ErrorCode::DeserializationError,
            Self::DuplicateRecordId => ErrorCode::ResourceAlreadyExists,
        }
    }
}

impl fmt::Display for RiskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code().name())
    }
}

impl std::error::Error for RiskError {}
