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
    /// A registration declared a price but no payout wallet. Billing
    /// without somewhere to be paid is a half-configured service, and
    /// the gap would only surface once money was owed.
    PricingWithoutPayoutWallet,
    /// An endpoint in a domain reserved by RFC 2606/6761, which can never
    /// resolve for anyone. See `registration::is_unresolvable`.
    UnresolvableEndpoint,
    /// Declared branding that is over a length bound, empty, would
    /// misrender, is a logo that is not a CID, or is a website that is
    /// not an ordinary http(s) address. See [`crate::ServiceBranding`].
    MalformedBranding,
    /// No outstanding earnings challenge matches this Service ID and
    /// nonce — never issued, already spent, or superseded.
    UnknownChallenge,
    /// The challenge was real but is past its TTL.
    ChallengeExpired,
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
            Self::PricingWithoutPayoutWallet => ErrorCode::InvalidRequest,
            Self::UnresolvableEndpoint => ErrorCode::InvalidParameter,
            Self::MalformedBranding => ErrorCode::InvalidParameter,
            Self::UnknownChallenge => ErrorCode::ResourceNotFound,
            Self::ChallengeExpired => ErrorCode::InvalidRequest,
        }
    }
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code().name())
    }
}

impl std::error::Error for RegistryError {}
