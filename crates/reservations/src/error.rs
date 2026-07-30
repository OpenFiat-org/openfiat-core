//! Reservation failures (OFS-2200 §21), mapped onto OFS-8000's
//! Reservation & Marketplace range (4000-4999) where a code exists there,
//! and the closest applicable code otherwise.

use openfiat_types::ErrorCode;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservationError {
    InvalidSignature,
    /// §21: an update signed by someone other than the reservation's owner.
    UnauthorizedUpdate,
    DuplicateReservationId,
    MalformedReservation,
    ReservationNotFound,
    AdvertisementNotFound,
    /// §21: negative or out-of-limits trade amounts.
    InvalidAmount,
    InsufficientLiquidity,
    /// §18: a cancel/extend was attempted on a reservation no longer in a
    /// live state.
    InvalidReservationState,
    /// The price the requester signed is not one this advertisement's
    /// terms produce.
    ///
    /// Refused rather than corrected to whatever the node thinks the price
    /// is. A reservation is an agreement, and silently substituting a
    /// different number would bind a taker to something they never signed
    /// — which is the whole failure this check exists to prevent, arrived
    /// at from the other direction.
    PriceDisagreement,
}

impl ReservationError {
    pub const fn code(self) -> ErrorCode {
        match self {
            Self::InvalidSignature => ErrorCode::InvalidSignature,
            Self::UnauthorizedUpdate => ErrorCode::InvalidIdentityClaim,
            Self::DuplicateReservationId => ErrorCode::ReservationAlreadyExists,
            Self::MalformedReservation => ErrorCode::DeserializationError,
            Self::ReservationNotFound => ErrorCode::ReservationNotFound,
            Self::AdvertisementNotFound => ErrorCode::AdvertisementNotFound,
            Self::InvalidAmount => ErrorCode::InvalidRequest,
            Self::PriceDisagreement => ErrorCode::PriceDisagreement,
            Self::InsufficientLiquidity => ErrorCode::InsufficientAvailableLiquidity,
            Self::InvalidReservationState => ErrorCode::InvalidReservationState,
        }
    }
}

impl fmt::Display for ReservationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code().name())
    }
}

impl std::error::Error for ReservationError {}
