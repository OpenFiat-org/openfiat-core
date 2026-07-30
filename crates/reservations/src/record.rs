//! The reservation shape and its local state machine (OFS-2200 §5-6, §18).

use openfiat_advertisements::AdvertisementId;
use openfiat_types::{Amount, PeerId, PublicKey, Timestamp};

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ReservationId(String);

impl ReservationId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// §18's state machine. This specification's authority — and this
/// crate's — ends at `EscrowLocked`; everything after that belongs to
/// Settlement (OFS-2300), a separate crate.
///
/// `Validated`/`Accepted` are momentary, not independently persisted:
/// this implementation validates synchronously, so a reservation is
/// stored either as `EscrowLocked` (validation succeeded) or not stored
/// at all (validation failed — §7 "only valid reservations proceed").
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ReservationState {
    EscrowLocked,
    Cancelled,
    Expired,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Reservation {
    pub id: ReservationId,
    pub advertisement_id: AdvertisementId,
    pub requester: PeerId,
    pub requester_public_key: PublicKey,
    pub amount: Amount,
    /// The fiat-per-asset price this reservation was made at, and the
    /// oracle mid behind it for a floating advertisement.
    ///
    /// This is the number the trade is actually for. The advertisement's
    /// own quote moves with the oracle and is only ever a display; once a
    /// reservation exists, the price is settled and stops moving.
    pub agreed_price: Amount,
    pub agreed_mid: Option<f64>,
    pub state: ReservationState,
    pub requested_at: Timestamp,
    pub updated_at: Timestamp,
    /// §12: the reservation-validation window deadline (30 minutes by
    /// default — see `crate::protocol::VALIDATION_WINDOW`).
    pub expires_at: Timestamp,
}
