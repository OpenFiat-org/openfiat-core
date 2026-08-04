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
    /// The one value a client displays.
    ///
    /// # Precedence: the settlement wins whenever there is one
    ///
    /// This used to match on the reservation first, and a `Cancelled` or
    /// `Expired` reservation short-circuited everything — so a trade whose
    /// settlement was running on toward `Completed` displayed as
    /// `Cancelled`, contradicting where the money was actually going. The
    /// data race behind it is now closed at the source (OFS-2300 §5a: a
    /// reservation a settlement holds is `Settling`, and neither its owner
    /// nor the expiry sweep can move it), but the ordering is what decides
    /// what a user sees when the two records still disagree, and they
    /// still can: expiry is computed against each node's own clock, so a
    /// node that swept a minute early holds an `Expired` reservation for a
    /// settlement its neighbours consider live.
    ///
    /// Settlement-first makes both nodes answer the same thing. It is also
    /// the right way round on the merits: OFS-2200 §18 hands authority to
    /// OFS-2300 §20 at `Escrow Locked`, and a reservation that has been
    /// handed over has nothing left to say about the trade. The
    /// reservation decides only while no settlement exists at all.
    pub fn status(&self) -> TradeStatus {
        if let Some(settlement) = &self.settlement {
            return match settlement.state {
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
                // Reachable at last. Nothing wrote this state, so a trade
                // in front of arbitrators displayed as `PaymentSubmitted`
                // — a merchant simply taking their time — for as long as
                // the case ran.
                SettlementState::Disputed => TradeStatus::Disputed,
            };
        }
        match self.reservation.state {
            ReservationState::EscrowLocked => TradeStatus::EscrowLocked,
            // A reservation that says a settlement holds it, on a node
            // that does not have that settlement. Not reachable through
            // the registries — the write that sets `Settling` is the one
            // that stores the settlement — but the honest answer if a
            // replica ever gets there is the last thing both records
            // agreed on: escrow is locked and the trade is under way.
            ReservationState::Settling => TradeStatus::EscrowLocked,
            ReservationState::Settled => TradeStatus::Completed,
            ReservationState::Cancelled | ReservationState::Expired => TradeStatus::Cancelled,
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

#[cfg(test)]
mod tests {
    use super::*;
    use openfiat_advertisements::AdvertisementId;
    use openfiat_crypto::Keypair;
    use openfiat_network::identity::peer_id_from_public_key;
    use openfiat_settlement::SettlementId;
    use openfiat_types::{Amount, Timestamp};

    fn trade(
        reservation_state: ReservationState,
        settlement_state: Option<SettlementState>,
    ) -> Trade {
        let buyer = Keypair::generate();
        let seller = Keypair::generate();
        let buyer_id = peer_id_from_public_key(&buyer.public_key()).unwrap();
        let seller_id = peer_id_from_public_key(&seller.public_key()).unwrap();
        Trade {
            reservation: Reservation {
                id: ReservationId::new("res-1"),
                advertisement_id: AdvertisementId::new("ad-1"),
                requester: buyer_id.clone(),
                requester_public_key: buyer.public_key(),
                amount: Amount::new(2_000_000, 6),
                agreed_price: Amount::new(129_000_000, 6),
                agreed_mid: None,
                state: reservation_state,
                requested_at: Timestamp::from_millis(1),
                updated_at: Timestamp::from_millis(1),
                expires_at: Timestamp::from_millis(2),
            },
            settlement: settlement_state.map(|state| Settlement {
                id: SettlementId::new("settle-1"),
                reservation_id: ReservationId::new("res-1"),
                buyer: buyer_id,
                buyer_public_key: buyer.public_key(),
                seller: seller_id,
                seller_public_key: seller.public_key(),
                amount: Amount::new(2_000_000, 6),
                state,
                payment_reference: None,
                escrow_release_signature: None,
                payment_submitted_at: None,
                merchant_responded_at: None,
                payment_discrepancy: None,
                disputed_at: None,
                created_at: Timestamp::from_millis(3),
                updated_at: Timestamp::from_millis(3),
            }),
        }
    }

    /// The precedence, stated as the case that used to get it wrong.
    ///
    /// Expiry is computed against each node's own clock, so a node that
    /// swept a minute early holds an `Expired` reservation for a
    /// settlement its neighbours consider live. Reservation-first made
    /// that node report `Cancelled` for a trade whose escrow was about to
    /// release, and made two honest nodes disagree about the same trade.
    #[test]
    fn a_settlement_decides_the_status_even_when_the_reservation_disagrees() {
        for stale in [ReservationState::Expired, ReservationState::Cancelled] {
            assert_eq!(
                trade(stale, Some(SettlementState::PaymentSubmitted)).status(),
                TradeStatus::PaymentSubmitted,
                "a running settlement is what the trade is doing"
            );
            assert_eq!(
                trade(stale, Some(SettlementState::Approved)).status(),
                TradeStatus::Completed,
            );
        }
    }

    /// The state that was declared and never written. A trade in front of
    /// arbitrators displayed as `PaymentSubmitted` — a merchant merely
    /// taking their time — for as long as the case ran.
    #[test]
    fn an_arbitrated_trade_says_so() {
        assert_eq!(
            trade(ReservationState::Settling, Some(SettlementState::Disputed)).status(),
            TradeStatus::Disputed
        );
    }

    #[test]
    fn the_reservation_decides_only_while_no_settlement_exists() {
        for (state, expected) in [
            (ReservationState::EscrowLocked, TradeStatus::EscrowLocked),
            (ReservationState::Settling, TradeStatus::EscrowLocked),
            (ReservationState::Settled, TradeStatus::Completed),
            (ReservationState::Cancelled, TradeStatus::Cancelled),
            (ReservationState::Expired, TradeStatus::Cancelled),
        ] {
            assert_eq!(trade(state, None).status(), expected);
        }
    }
}
