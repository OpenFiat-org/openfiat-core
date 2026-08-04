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
    /// The `id` on the envelope is not the id §5 says that envelope's own
    /// content computes to.
    ///
    /// The id is the dedup key, and until this check existed it was
    /// whatever the sender wrote in the field — nothing derived it from
    /// the content it names. Anyone relaying a genuinely signed event
    /// could therefore mint unlimited *distinct* copies of it by varying
    /// only the id, each one passing the signature check (the signature
    /// covers everything but the id), each one a fresh entry in every
    /// peer's dedup store, and each one re-forwarded. That is not a
    /// replay the store can recognise, because the store recognises ids.
    EventIdMismatch,
    /// Stamped further into the future than any clock disagreement
    /// explains.
    ///
    /// The event log is pruned by timestamp, so a far-future stamp is a
    /// permanent entry: it is never older than any cutoff and never
    /// swept. Refusing it is what keeps "the log holds a bounded window"
    /// true against a sender who chooses the field.
    TimestampTooFarAhead,
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
            // An envelope whose id is not its content's id is malformed in
            // the one way that matters: the field the protocol indexes it
            // by does not describe it.
            Self::EventIdMismatch => ErrorCode::DeserializationError,
            Self::TimestampTooFarAhead => ErrorCode::InvalidRequest,
            // 2006, not `InvalidSignature` (1003). The old mapping
            // reasoned that an origin which cannot be what it claims is
            // a signature that fails to establish what it appears to —
            // but the signature did establish it, correctly, which is
            // the whole problem. A peer told 1003 re-signs and resends;
            // an operator told 1003 goes looking for a bug in signing.
            // Neither of those is the remedy, and 2006 names the one
            // that is.
            Self::IdentityInUseElsewhere => ErrorCode::IdentityInUseElsewhere,
        }
    }
}

impl fmt::Display for GossipError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code().name())
    }
}

impl std::error::Error for GossipError {}
