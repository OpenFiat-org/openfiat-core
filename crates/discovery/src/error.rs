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
            // No dedicated "invalid public key" code exists; this falls
            // back to the same category a bad signature would.
            Self::InvalidPublicKey => ErrorCode::InvalidSignature,
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
