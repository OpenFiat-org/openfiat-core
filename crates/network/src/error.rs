//! The OFNP §22 error categories, mapped onto OFS-8000 codes.
//!
//! OFNP names its own error categories at a transport-implementer level
//! (`Invalid Message`, `Sequence Violation`, ...); OFS-8000 defines the
//! actual cross-transport numeric registry. The mapping isn't always 1:1 —
//! OFS-8000's network range (1000-1999) doesn't have a dedicated code for
//! every OFNP category — so a couple of these fall back to the nearest
//! applicable general code, noted per-variant below.

use openfiat_types::ErrorCode;
use std::fmt;

/// A transport-layer failure, categorized per OFNP §22.
#[derive(Debug)]
pub enum NetworkError {
    InvalidMessage,
    UnsupportedProtocol,
    AuthenticationFailure,
    AuthorizationFailure,
    MalformedPayload,
    DuplicateSequence,
    OutOfOrderSequence,
    Timeout,
    ResourceExhaustion,
    Internal,
}

impl NetworkError {
    /// The closest OFS-8000 code for this category.
    pub const fn code(&self) -> ErrorCode {
        match self {
            // No dedicated "invalid message" code exists; this is the
            // general-purpose fallback OFS-8000 itself provides for it.
            Self::InvalidMessage => ErrorCode::InvalidRequest,
            Self::UnsupportedProtocol => ErrorCode::ProtocolVersionMismatch,
            Self::AuthenticationFailure => ErrorCode::InvalidSignature,
            // Same fallback rationale as InvalidMessage — OFS-8000 has no
            // network-range authorization code.
            Self::AuthorizationFailure => ErrorCode::InvalidRequest,
            Self::MalformedPayload => ErrorCode::DeserializationError,
            // A duplicate is specifically what OGP/ONSP call a replay.
            Self::DuplicateSequence => ErrorCode::ReplayAttackDetected,
            Self::OutOfOrderSequence => ErrorCode::MessageOutOfOrder,
            Self::Timeout => ErrorCode::OperationTimeout,
            // Imprecise and knowingly kept. A rate limit is a speed and
            // exhaustion is a level, the distinction that earned
            // `PaymentMethodLimitReached` (3006) its own code — but the
            // two arguments do not land the same way. 3006 was a
            // permanent cap wearing a retryable code, so a client backed
            // off forever over something no amount of waiting fixed.
            // These two are both retryable and both truthfully so: a
            // transport out of buffers and a caller over quota are both
            // relieved by waiting, and every client's next move is the
            // same backoff.
            //
            // The variant also has no construction site anywhere in this
            // workspace — OFNP §22 names the category, and nothing in
            // the transport raises it yet. A code minted for it today
            // would enter the registry with no wire evidence behind it.
            // Worth revisiting when backpressure is actually wired up,
            // because "the node is full, try another peer" and "you are
            // over quota, slow down" do diverge once a client has more
            // than one peer to choose from.
            Self::ResourceExhaustion => ErrorCode::RateLimitExceeded,
            Self::Internal => ErrorCode::InternalError,
        }
    }
}

impl fmt::Display for NetworkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code().name())
    }
}

impl std::error::Error for NetworkError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_sequence_maps_to_replay_attack_detected() {
        assert_eq!(
            NetworkError::DuplicateSequence.code(),
            ErrorCode::ReplayAttackDetected
        );
    }
}
