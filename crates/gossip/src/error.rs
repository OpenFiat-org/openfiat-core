//! Gossip validation failures (OGP §9, §23), mapped onto OFS-8000 codes.

use openfiat_types::ErrorCode;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GossipError {
    InvalidSignature,
    /// §7: "Node implementations MUST reject unauthorized event types."
    UnauthorizedOrigination,
    ProtocolVersionMismatch,
    MalformedPayload,
}

impl GossipError {
    pub const fn code(self) -> ErrorCode {
        match self {
            Self::InvalidSignature => ErrorCode::InvalidSignature,
            // No dedicated "unauthorized event type" code exists in the
            // network range; this is the general-purpose rejection code.
            Self::UnauthorizedOrigination => ErrorCode::InvalidRequest,
            Self::ProtocolVersionMismatch => ErrorCode::ProtocolVersionMismatch,
            Self::MalformedPayload => ErrorCode::DeserializationError,
        }
    }
}

impl fmt::Display for GossipError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code().name())
    }
}

impl std::error::Error for GossipError {}
