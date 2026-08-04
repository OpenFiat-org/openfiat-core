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
///
/// `Settling` and `Settled` are the two states this crate defines but
/// never writes: OFS-2300 §5a owns both, because they say what the
/// *settlement* has done to a reservation it took authority over.
/// `openfiat-settlement` writes them through
/// [`crate::ReservationRegistry::settlement_started`] and its two
/// conclusion counterparts, which is the direction the dependency already
/// runs — this crate knows nothing about settlements and gains no
/// dependency here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ReservationState {
    EscrowLocked,
    /// A settlement has been raised against this reservation and has not
    /// concluded (OFS-2300 §5a).
    ///
    /// The reason this is a state rather than a lookup: the reservation
    /// and the settlement are two independently replicated records, and
    /// while they were only correlated at read time a taker could cancel
    /// the reservation out from under a settlement that was already
    /// running — crediting the merchant's advertisement with liquidity
    /// committed to a live trade, and leaving the trade view contradicting
    /// where the money actually went. Cancellation is refused from here
    /// ([`crate::ReservationError::SettlementInFlight`]) and the expiry
    /// sweep skips it, so the liquidity stays committed for exactly as
    /// long as the settlement holds it.
    Settling,
    /// The settlement concluded with the escrow moving (OFS-2300 §5a) —
    /// terminal, and deliberately *not* a state that returns liquidity.
    ///
    /// The asset was sold. Crediting the advertisement again here would
    /// invent inventory the merchant no longer has, which is what the
    /// expiry sweep did to every completed trade for as long as a
    /// completed reservation sat in `EscrowLocked` waiting to go stale.
    Settled,
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
