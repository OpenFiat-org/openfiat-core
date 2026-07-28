//! Registry failures (OFS-1500 §21), mapped onto OFS-8000 codes.
//!
//! OFS-8000 has no dedicated error range for the Service Registry — its
//! range table stops at Notifications (8000-8999) and Internal
//! (9000-9999) without ever allocating one for SRP. Each variant here maps
//! to the closest existing general/network-range code instead of
//! inventing an unregistered one.

use openfiat_types::ErrorCode;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryError {
    InvalidSignature,
    /// §21: an update/health-update/withdrawal signed by someone other
    /// than the service's original registrant.
    UnauthorizedUpdate,
    /// §21: a registration claims a Service ID already owned by a
    /// different provider.
    DuplicateServiceId,
    MalformedRegistration,
    ServiceNotFound,
    /// The registry accepted the change locally but gossip refused to
    /// originate it (e.g. this node isn't authorized to emit the event
    /// type at all, per OGP §7).
    GossipRejected,
}

impl RegistryError {
    pub const fn code(self) -> ErrorCode {
        match self {
            Self::InvalidSignature => ErrorCode::InvalidSignature,
            Self::UnauthorizedUpdate => ErrorCode::InvalidIdentityClaim,
            Self::DuplicateServiceId => ErrorCode::ResourceAlreadyExists,
            Self::MalformedRegistration => ErrorCode::DeserializationError,
            Self::ServiceNotFound => ErrorCode::ResourceNotFound,
            Self::GossipRejected => ErrorCode::InvalidRequest,
        }
    }
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code().name())
    }
}

impl std::error::Error for RegistryError {}
