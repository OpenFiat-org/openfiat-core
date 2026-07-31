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
    /// This node holds no checkpoint of its own, and the snapshot offered
    /// to it comes from a producer outside [`crate::trust::TrustAnchors`].
    ///
    /// Distinct from [`Self::Unauthorized`] because it says something
    /// different and is fixed differently: the producer may be perfectly
    /// well registered. What is missing is any basis for *this* node to
    /// believe them, since it has no history to judge the snapshot
    /// against — see `crate::trust` for why a first snapshot is the one
    /// case where registration is not enough.
    UntrustedFirstSnapshot,
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
    /// A location that isn't an absolute `http`/`https` URL — see
    /// `crate::location` for why nothing else is entertained.
    InvalidLocation,
    /// This node was asked to produce a snapshot but has no publicly
    /// reachable URL configured, so it has nowhere to tell peers to fetch
    /// one from.
    NoLocationConfigured,
    /// §8: the announcement carries no location at all. Only reachable
    /// for records produced by an older or foreign implementation.
    NoLocationAdvertised,
    /// §13: this node has no verified announcement for that Snapshot ID,
    /// so there is nothing it is willing to fetch or import.
    UnknownSnapshot,
    /// §14: no announced location produced usable bytes.
    DownloadFailed,
    /// §18: the snapshot is at or below the height this node already
    /// holds. Importing it would overwrite newer state with older.
    StaleSnapshot,
    /// The snapshot tried to write a column family that is this node's
    /// own snapshot bookkeeping — see `crate::state::restore`.
    ReservedColumnFamily,
    /// An entry in the snapshot failed the importing node's own check on
    /// what that column family may contain — see
    /// [`crate::state::EntryVerifier`].
    ///
    /// Distinct from [`Self::StateRootMismatch`], which it can coexist
    /// with only in the sense that both mean "do not import this". The
    /// state root proves the blob is what the producer announced; it says
    /// nothing about whether the producer put honest contents inside it.
    UnverifiableEntry,
    /// The snapshot is larger than this node will hold in memory —
    /// [`crate::codec::MAX_SNAPSHOT_BYTES`].
    SnapshotTooLarge,
    /// A column family could not be read while assembling a snapshot —
    /// an incomplete snapshot is never produced in its place.
    StateUnreadable,
    /// Imported state, or a produced snapshot file, could not be written.
    StateUnwritable,
}

impl SnapshotError {
    pub const fn code(self) -> ErrorCode {
        match self {
            Self::InvalidSignature => ErrorCode::InvalidSignature,
            Self::Unauthorized => ErrorCode::InvalidRequest,
            // Not InvalidRequest: the request is well formed and the
            // producer may be properly registered. What is refused is this
            // node trusting it, which is a state of the node rather than a
            // fault in the message.
            Self::UntrustedFirstSnapshot => ErrorCode::SnapshotVerificationFailed,
            Self::MalformedRecord => ErrorCode::DeserializationError,
            Self::DuplicateSnapshotId => ErrorCode::ResourceAlreadyExists,
            Self::UnsupportedCompression => ErrorCode::UnsupportedOperation,
            Self::UnsupportedProtocolVersion => ErrorCode::ProtocolVersionMismatch,
            Self::SizeMismatch => ErrorCode::SnapshotVerificationFailed,
            Self::StateRootMismatch => ErrorCode::SnapshotVerificationFailed,
            Self::InvalidLocation => ErrorCode::InvalidParameter,
            Self::NoLocationConfigured => ErrorCode::InvalidRequest,
            Self::NoLocationAdvertised => ErrorCode::ResourceNotFound,
            Self::UnknownSnapshot => ErrorCode::ResourceNotFound,
            Self::DownloadFailed => ErrorCode::NetworkError,
            Self::StaleSnapshot => ErrorCode::InvalidRequest,
            Self::ReservedColumnFamily => ErrorCode::SnapshotVerificationFailed,
            Self::UnverifiableEntry => ErrorCode::SnapshotVerificationFailed,
            Self::SnapshotTooLarge => ErrorCode::SnapshotVerificationFailed,
            Self::StateUnreadable => ErrorCode::DatabaseError,
            Self::StateUnwritable => ErrorCode::DatabaseError,
        }
    }
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code().name())
    }
}

impl std::error::Error for SnapshotError {}
