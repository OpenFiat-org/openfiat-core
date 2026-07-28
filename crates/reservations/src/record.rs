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
    pub state: ReservationState,
    pub requested_at: Timestamp,
    pub updated_at: Timestamp,
    /// §12: the reservation-validation window deadline (30 minutes by
    /// default — see `crate::protocol::VALIDATION_WINDOW`).
    pub expires_at: Timestamp,
}
