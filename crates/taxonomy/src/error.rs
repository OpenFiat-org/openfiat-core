//! Payment-method failures, mapped onto OFS-8000's advertisement range —
//! `UNSUPPORTED_PAYMENT_METHOD` is the code that already exists there for
//! "this rail is not one this node will carry".

use openfiat_types::ErrorCode;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaxonomyError {
    InvalidSignature,
    /// The name is not one a client can safely render, or the record is
    /// otherwise misshapen. See [`crate::name::check_name`] for the whole
    /// list of what that covers and why each entry is on it.
    MalformedDefinition,
    /// The name reduces to the same skeleton as a rail this build already
    /// ships — a look-alike, whatever it is spelled with.
    ImpersonatesKnownMethod,
    /// This merchant already has [`crate::store::MAX_METHODS_PER_MERCHANT`]
    /// definitions on file and this one does not displace any of them.
    TooManyMethods,
}

impl TaxonomyError {
    pub const fn code(self) -> ErrorCode {
        match self {
            Self::InvalidSignature => ErrorCode::InvalidSignature,
            Self::MalformedDefinition | Self::ImpersonatesKnownMethod => {
                ErrorCode::UnsupportedPaymentMethod
            }
            // Not `RateLimitExceeded`, which is where this used to land.
            // A rate limit is a speed and every client that handles one
            // handles it by waiting; [`crate::store::
            // MAX_METHODS_PER_MERCHANT`] is a count that does not decay.
            // Nothing frees a slot but the merchant retiring a
            // definition, so a caller told to back off backs off forever.
            Self::TooManyMethods => ErrorCode::PaymentMethodLimitReached,
        }
    }
}

impl fmt::Display for TaxonomyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code().name())
    }
}

impl std::error::Error for TaxonomyError {}
