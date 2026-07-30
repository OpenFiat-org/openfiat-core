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
    /// An event signed by *this node's own key* that this node did not
    /// emit — proof that another process holds the same identity.
    ///
    /// Rejected rather than stored: acting on an instruction issued under
    /// our name by someone else is the one thing a node must never do,
    /// and a duplicated identity is a compromised or copied `wallet.json`
    /// either way.
    IdentityInUseElsewhere,
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
            // The closest existing code: an event whose origin cannot be
            // what it claims is exactly a signature that does not
            // establish what it appears to.
            Self::IdentityInUseElsewhere => ErrorCode::InvalidSignature,
        }
    }
}

impl fmt::Display for GossipError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code().name())
    }
}

impl std::error::Error for GossipError {}
