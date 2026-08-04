//! Discovery-layer failures, mapped onto OFS-8000 codes.

use openfiat_types::ErrorCode;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryError {
    MalformedAdvertisement,
    InvalidPublicKey,
    /// The advertisement's claimed Peer ID doesn't match the Peer ID its
    /// own public key derives to — the OFS-1100 §21/§25 "peer poisoning"
    /// case.
    PeerIdMismatch,
    InvalidSignature,
}

impl DiscoveryError {
    pub const fn code(self) -> ErrorCode {
        match self {
            Self::MalformedAdvertisement => ErrorCode::DeserializationError,
            // Raised before any signature is looked at: the
            // advertisement's `public_key` field is not a key a Peer ID
            // can be derived from. `InvalidSignature` (1003), which this
            // used to answer with, describes a check that has not run
            // yet and points a publisher at their signing code instead
            // of at the malformed field. `InvalidParameter` is the plain
            // statement — one field of the record is not a value this
            // protocol accepts — and is what `openfiat_content` uses for
            // the same shape of failure.
            Self::InvalidPublicKey => ErrorCode::InvalidParameter,
            Self::PeerIdMismatch => ErrorCode::InvalidIdentityClaim,
            Self::InvalidSignature => ErrorCode::InvalidSignature,
        }
    }
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code().name())
    }
}

impl std::error::Error for DiscoveryError {}
