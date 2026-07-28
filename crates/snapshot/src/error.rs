//! Snapshot failures, mapped onto OFS-8000 codes. `SnapshotVerificationFailed`
//! (1005, Network range) is an exact fit for §10/§16's verification
//! failures; everything else maps to the closest general-range code,
//! the same approach `openfiat-registry`/`openfiat-oracles`/`openfiat-risk`
//! take for specs OFS-8000 allocates no dedicated range for.

use openfiat_types::ErrorCode;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotError {
    InvalidSignature,
    /// §5/§24: the announcer isn't registered as a snapshot provider in
    /// `openfiat-registry`.
    Unauthorized,
    MalformedRecord,
    /// §24: a Snapshot ID that's already on file.
    DuplicateSnapshotId,
    /// §16: `compression` isn't one this crate can actually decompress.
    UnsupportedCompression,
    /// §16: `protocol_version` isn't one this node understands.
    UnsupportedProtocolVersion,
    /// §8/§16: the downloaded bytes' length doesn't match `size_bytes`.
    SizeMismatch,
    /// §10/§16: the decompressed state's digest doesn't match `state_root`.
    StateRootMismatch,
}

impl SnapshotError {
    pub const fn code(self) -> ErrorCode {
        match self {
            Self::InvalidSignature => ErrorCode::InvalidSignature,
            Self::Unauthorized => ErrorCode::InvalidRequest,
            Self::MalformedRecord => ErrorCode::DeserializationError,
            Self::DuplicateSnapshotId => ErrorCode::ResourceAlreadyExists,
            Self::UnsupportedCompression => ErrorCode::UnsupportedOperation,
            Self::UnsupportedProtocolVersion => ErrorCode::ProtocolVersionMismatch,
            Self::SizeMismatch => ErrorCode::SnapshotVerificationFailed,
            Self::StateRootMismatch => ErrorCode::SnapshotVerificationFailed,
        }
    }
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code().name())
    }
}

impl std::error::Error for SnapshotError {}
