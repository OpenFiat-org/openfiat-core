//! The trade view (OFS-2000 §9): "Every trade is the composition of two
//! sub-protocols, each with its own canonical state machine... This
//! specification does not redefine either state machine."
//!
//! Because of that, this crate has no state machine, no signed events,
//! and no gossip origination of its own — a `Trade` is purely a read-time
//! join of a `Reservation` (owned by `openfiat-reservations`) and its
//! `Settlement`, if one has started (owned by `openfiat-settlement`),
//! correlated by `ReservationId`.

use openfiat_reservations::{Reservation, ReservationId, ReservationRegistry, ReservationState};
use openfiat_settlement::{Settlement, SettlementRegistry, SettlementState};
use openfiat_storage::KvStore;
use std::rc::Rc;

/// The aggregate status a client actually wants to display — one value
/// instead of "check the reservation state, then check whether a
/// settlement exists, then check its state".
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TradeStatus {
    /// Reservation succeeded (§18); settlement hasn't started yet.
    EscrowLocked,
    AwaitingPayment,
    PaymentSubmitted,
    Completed,
    Rejected,
    Cancelled,
    Disputed,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Trade {
    pub reservation: Reservation,
    pub settlement: Option<Settlement>,
}

impl Trade {
    pub fn status(&self) -> TradeStatus {
        match (self.reservation.state, &self.settlement) {
            (ReservationState::Cancelled, _) | (ReservationState::Expired, _) => {
                TradeStatus::Cancelled
            }
            (ReservationState::EscrowLocked, None) => TradeStatus::EscrowLocked,
            (ReservationState::EscrowLocked, Some(settlement)) => match settlement.state {
                SettlementState::AwaitingPayment => TradeStatus::AwaitingPayment,
                SettlementState::PaymentSubmitted => TradeStatus::PaymentSubmitted,
                // `Approved` (merchant approved, on-chain release not yet
                // confirmed) and `Completed` (release confirmed, OFS-4300)
                // are a real, distinct pair now — but a client displaying
                // trade status still only needs one value for both; the
                // settlement's own `escrow_release_signature` is where
                // "has it actually landed on-chain yet" lives for a caller
                // that cares about that distinction.
                SettlementState::Approved | SettlementState::Completed => TradeStatus::Completed,
                SettlementState::Rejected => TradeStatus::Rejected,
                SettlementState::Cancelled => TradeStatus::Cancelled,
                SettlementState::Disputed => TradeStatus::Disputed,
            },
        }
    }
}

/// Reads a consistent trade view from the shared reservation and
/// settlement registries a node already maintains — see
/// `ReservationService::registry`/`SettlementService::registry`.
pub struct TradeView<S> {
    reservations: Rc<ReservationRegistry<S>>,
    settlements: Rc<SettlementRegistry<S>>,
}

impl<S: KvStore> TradeView<S> {
    pub fn new(
        reservations: Rc<ReservationRegistry<S>>,
        settlements: Rc<SettlementRegistry<S>>,
    ) -> Self {
        Self {
            reservations,
            settlements,
        }
    }

    pub fn get(&self, reservation_id: &ReservationId) -> Option<Trade> {
        let reservation = self.reservations.get(reservation_id)?;
        let settlement = self
            .settlements
            .all()
            .into_iter()
            .find(|settlement| &settlement.reservation_id == reservation_id);
        Some(Trade {
            reservation,
            settlement,
        })
    }

    /// Every trade this node currently knows about.
    ///
    /// O(reservations × settlements) — fine at the scale a single node's
    /// local replica holds; if that stops being true, index settlements
    /// by `ReservationId` instead of scanning for the match.
    pub fn all(&self) -> Vec<Trade> {
        let settlements = self.settlements.all();
        self.reservations
            .all()
            .into_iter()
            .map(|reservation| {
                let settlement = settlements
                    .iter()
                    .find(|settlement| settlement.reservation_id == reservation.id)
                    .cloned();
                Trade {
                    reservation,
                    settlement,
                }
            })
            .collect()
    }
}
