//! The replicated local reservation index — the same shape as every
//! other store in this workspace, plus a shared handle to the
//! advertisement registry it validates against and adjusts (§9-10, §15).

use crate::error::ReservationError;
use crate::events::{SignedReservationCancel, SignedReservationRequest};
use crate::protocol;
use crate::record::{Reservation, ReservationId, ReservationState};
use openfiat_advertisements::{AdvertisementRegistry, AdvertisementStatus};
use openfiat_serialization::json;
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{EventEnvelope, Timestamp};
use std::rc::Rc;
use std::time::Duration;

const COLUMN_FAMILY: &str = "reservations";

/// What one pass of [`ReservationRegistry::expire_stale`] did.
///
/// Two numbers rather than one because they mean opposite things to an
/// operator: `expired` is the sweep working, `deferred` is the sweep
/// finding a reservation it was unable to unwind and choosing to leave it
/// locked rather than expire it without returning the liquidity. A single
/// count would have hidden the second inside the first.
///
/// Both zero is the ordinary result — most passes find nothing — so a
/// caller driving this on a timer should say nothing at all in that case
/// rather than emit a line per tick that buries the passes that matter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExpirySweep {
    /// Reservations moved to `Expired`, their liquidity returned to the
    /// advertisement.
    pub expired: usize,
    /// Reservations past their deadline whose advertisement could not be
    /// credited, left in `EscrowLocked` for the next sweep to retry.
    /// Always zero on a healthy node.
    pub deferred: usize,
}

pub struct ReservationRegistry<S> {
    store: S,
    advertisements: Rc<AdvertisementRegistry<S>>,
}

impl<S: KvStore> ReservationRegistry<S> {
    pub fn new(store: S, advertisements: Rc<AdvertisementRegistry<S>>) -> Self {
        Self {
            store,
            advertisements,
        }
    }

    pub fn get(&self, id: &ReservationId) -> Option<Reservation> {
        let bytes = self
            .store
            .get(COLUMN_FAMILY, id.as_str().as_bytes())
            .ok()
            .flatten()?;
        wire::from_bytes(&bytes).ok()
    }

    fn put(&self, reservation: &Reservation) {
        if let Ok(bytes) = wire::to_bytes(reservation) {
            let _ = self
                .store
                .put(COLUMN_FAMILY, reservation.id.as_str().as_bytes(), &bytes);
        }
    }

    pub fn all(&self) -> Vec<Reservation> {
        self.store
            .iter_prefix(COLUMN_FAMILY, &[])
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(_, value)| wire::from_bytes(&value).ok())
            .collect()
    }

    /// §6-10: validate a request against the (already-synchronized)
    /// advertisement it targets and, if it clears every check, lock the
    /// corresponding liquidity and record the reservation as
    /// `EscrowLocked`. §7: "only valid reservations proceed" — nothing is
    /// stored for a rejected request.
    pub fn apply_request(
        &self,
        signed: SignedReservationRequest,
    ) -> Result<ReservationId, ReservationError> {
        signed.verify()?;
        let id = signed.request.id.clone();
        if self.get(&id).is_some() {
            return Err(ReservationError::DuplicateReservationId);
        }
        if signed.request.amount.base_units() == 0 {
            return Err(ReservationError::InvalidAmount);
        }

        // The requester signs their own timestamp, and everything about
        // how long this reservation lives is derived from it — both the
        // `expires_at` stored below and the deadline `expire_stale`
        // enforces. A signature says who wrote the number, not that the
        // number is true, so an unbounded one lets a taker mint
        // themselves an arbitrarily long validation window and hold a
        // merchant's liquidity for as long as they like.
        //
        // Checked here, before `reserve_liquidity`, so a refused request
        // never moves a merchant's balance even briefly.
        if signed.request.timestamp.as_millis()
            > Timestamp::now().as_millis() + protocol::MAX_CLOCK_SKEW.as_millis() as u64
        {
            return Err(ReservationError::TimestampTooFarAhead);
        }

        let ad = self
            .advertisements
            .get(&signed.request.advertisement_id)
            .ok_or(ReservationError::AdvertisementNotFound)?;
        if ad.status != AdvertisementStatus::Active {
            return Err(ReservationError::AdvertisementNotFound);
        }
        if signed.request.amount.base_units() < ad.min_trade.base_units()
            || signed.request.amount.base_units() > ad.max_trade.base_units()
        {
            return Err(ReservationError::InvalidAmount);
        }

        // The price the taker signed must be one this advertisement's own
        // terms produce. For a fixed ad that is an exact match against the
        // merchant's signed number; for a floating one it is the formula
        // applied to the mid the taker recorded.
        //
        // Deliberately not compared against this node's own oracle view:
        // two honest nodes hold different records and would accept
        // different reservations, so the same user would succeed or fail
        // depending on which access node they picked. What every node can
        // check is that the arithmetic follows.
        if !ad
            .pricing
            .agrees_with(signed.request.agreed_price, signed.request.agreed_mid)
        {
            return Err(ReservationError::PriceDisagreement);
        }

        self.advertisements
            .reserve_liquidity(&signed.request.advertisement_id, signed.request.amount)
            .map_err(|_| ReservationError::InsufficientLiquidity)?;

        let expires_at = Timestamp::from_millis(
            signed.request.timestamp.as_millis() + protocol::VALIDATION_WINDOW.as_millis() as u64,
        );
        self.put(&Reservation {
            id: id.clone(),
            advertisement_id: signed.request.advertisement_id,
            requester: signed.request.requester,
            requester_public_key: signed.request.requester_public_key,
            amount: signed.request.amount,
            agreed_price: signed.request.agreed_price,
            agreed_mid: signed.request.agreed_mid,
            state: ReservationState::EscrowLocked,
            requested_at: signed.request.timestamp,
            updated_at: signed.request.timestamp,
            expires_at,
        });
        Ok(id)
    }

    /// §14: before escrow is locked cancellation is unrestricted; this
    /// crate's scope ends at `EscrowLocked` itself, so any reservation
    /// still in that state may still be cancelled here (post-payment
    /// cancellation rules belong to OFS-2300 §19, once settlement takes
    /// over).
    pub fn apply_cancel(&self, signed: SignedReservationCancel) -> Result<(), ReservationError> {
        let mut reservation = self
            .get(&signed.cancel.id)
            .ok_or(ReservationError::ReservationNotFound)?;
        if reservation.requester != signed.cancel.requester {
            return Err(ReservationError::UnauthorizedUpdate);
        }
        let bytes =
            json::to_bytes(&signed.cancel).map_err(|_| ReservationError::MalformedReservation)?;
        openfiat_crypto::verify(&reservation.requester_public_key, &bytes, &signed.signature)
            .map_err(|_| ReservationError::InvalidSignature)?;
        if reservation.state != ReservationState::EscrowLocked {
            return Err(ReservationError::InvalidReservationState);
        }

        let _ = self
            .advertisements
            .release_liquidity(&reservation.advertisement_id, reservation.amount);
        reservation.state = ReservationState::Cancelled;
        reservation.updated_at = signed.cancel.timestamp;
        self.put(&reservation);
        Ok(())
    }

    /// §12/§12a: reservations past their validation-window deadline
    /// expire automatically, purely as local bookkeeping — every node
    /// computes this independently from timestamps it already has, no
    /// gossip event required.
    ///
    /// **Node-local by construction, and deliberately not a gossip
    /// event.** The deadline is a pure function of data every replica
    /// already holds: `requested_at`, which the requester signed and which
    /// travels inside the replicated `ReservationRequested` event, plus
    /// this build's [`protocol::VALIDATION_WINDOW`]. An "expired" event
    /// would carry no bit a receiving node could not derive, and would
    /// make the outcome depend on delivery — a node that missed the
    /// message would hold the liquidity forever, which is precisely the
    /// failure this sweep exists to end. It also has no honest signer: the
    /// requester will not sign away their own reservation, a merchant
    /// signing it could expire reservations early, and a third party
    /// signing it needs a quorum OFS-2200 does not have.
    ///
    /// Nodes therefore converge without agreeing on anything, bounded by
    /// clock skew plus [`protocol::SWEEP_INTERVAL`] — see that constant
    /// for why the bound is chosen as tightly as it is. Ordering against a
    /// concurrent cancel converges too: whichever lands first moves the
    /// reservation out of `EscrowLocked`, and the other is refused
    /// (`apply_cancel` requires that state, this loop skips anything not
    /// in it), so the liquidity is returned exactly once on every node.
    ///
    /// A reservation is only marked `Expired` if its liquidity actually
    /// came back. Expiring one whose `release_liquidity` failed would
    /// destroy the merchant's inventory silently — worse than leaving it
    /// locked, because a locked reservation is swept again next tick and
    /// heals itself if the missing advertisement later arrives. Those are
    /// reported as [`ExpirySweep::deferred`] so the caller can say so out
    /// loud rather than discovering it as a number that does not add up.
    pub fn expire_stale(&self, window: Duration) -> ExpirySweep {
        let cutoff = Timestamp::now()
            .as_millis()
            .saturating_sub(window.as_millis() as u64);
        let mut outcome = ExpirySweep::default();
        for mut reservation in self.all() {
            if reservation.state != ReservationState::EscrowLocked
                || reservation.requested_at.as_millis() >= cutoff
            {
                continue;
            }
            if self
                .advertisements
                .release_liquidity(&reservation.advertisement_id, reservation.amount)
                .is_err()
            {
                outcome.deferred += 1;
                continue;
            }
            reservation.state = ReservationState::Expired;
            reservation.updated_at = Timestamp::now();
            self.put(&reservation);
            outcome.expired += 1;
        }
        outcome
    }

    pub fn apply_event(&self, event: &EventEnvelope) {
        if event.ofs_spec != protocol::OFS_SPEC {
            return;
        }
        match event.event_type.as_str() {
            protocol::EVENT_REQUESTED => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_request(signed);
                }
            }
            protocol::EVENT_CANCELLED => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_cancel(signed);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{ReservationCancel, ReservationRequest};
    use openfiat_advertisements::events::{AdvertisementCreate, SignedAdvertisementCreate};
    use openfiat_advertisements::{AdvertisementId, Direction, PricingModel};
    use openfiat_crypto::{Keypair, MintAddress};
    use openfiat_network::identity::peer_id_from_public_key;
    use openfiat_storage::mem::MemoryStore;
    use openfiat_types::Amount;
    use openfiat_types::FiatCurrency;

    fn setup() -> (
        Rc<AdvertisementRegistry<MemoryStore>>,
        ReservationRegistry<MemoryStore>,
        Keypair,
        AdvertisementId,
    ) {
        let advertisements = Rc::new(AdvertisementRegistry::new(MemoryStore::new()));
        let merchant = Keypair::generate();
        let ad_id = AdvertisementId::new("ad-1");
        let create = AdvertisementCreate {
            id: ad_id.clone(),
            merchant: peer_id_from_public_key(&merchant.public_key()).unwrap(),
            merchant_public_key: merchant.public_key(),
            asset_mint: MintAddress::parse("2bHPi5hA4zrmPAfrvLmEexg3KJjpTjNkUcxWnzUPeRRU").unwrap(),
            direction: Direction::Sell,
            fiat_currency: FiatCurrency::parse("KES").unwrap(),
            min_trade: Amount::new(1_000_000, 6),
            max_trade: Amount::new(5_000_000, 6),
            initial_liquidity: Amount::new(10_000_000, 6),
            pricing: PricingModel::Fixed {
                price: Amount::new(129_000_000, 6),
            },
            payment_methods: vec!["Mobile Money".to_string()],
            timestamp: Timestamp::now(),
        };
        advertisements
            .apply_create(SignedAdvertisementCreate::sign(create, &merchant))
            .unwrap();

        let reservations = ReservationRegistry::new(MemoryStore::new(), Rc::clone(&advertisements));
        (advertisements, reservations, merchant, ad_id)
    }

    fn request(
        buyer: &Keypair,
        id: &str,
        ad_id: &AdvertisementId,
        amount: u64,
    ) -> ReservationRequest {
        ReservationRequest {
            id: ReservationId::new(id),
            advertisement_id: ad_id.clone(),
            requester: peer_id_from_public_key(&buyer.public_key()).unwrap(),
            requester_public_key: buyer.public_key(),
            amount: Amount::new(amount, 6),
            // The advertisement above is fixed-price, so the only price a
            // reservation against it can carry is the merchant's own.
            agreed_price: Amount::new(129_000_000, 6),
            agreed_mid: None,
            timestamp: Timestamp::now(),
        }
    }

    /// The number a taker signs is the number the trade is for.
    #[test]
    fn a_reservation_records_the_price_it_was_made_at() {
        let (_ads, reservations, _merchant, ad_id) = setup();
        let buyer = Keypair::generate();
        let id = reservations
            .apply_request(SignedReservationRequest::sign(
                request(&buyer, "res-priced", &ad_id, 2_000_000),
                &buyer,
            ))
            .expect("a request at the advertised price is accepted");

        let stored = reservations.get(&id).unwrap();
        assert_eq!(stored.agreed_price, Amount::new(129_000_000, 6));
        assert_eq!(stored.agreed_mid, None, "a fixed ad derives from nothing");
    }

    /// Before this field existed, a taker agreed to a number the protocol
    /// recorded nowhere, so a merchant could later assert a different rate
    /// with nothing to contradict them.
    #[test]
    fn a_price_the_merchant_never_offered_is_refused() {
        let (_ads, reservations, _merchant, ad_id) = setup();
        let buyer = Keypair::generate();
        let mut cheeky = request(&buyer, "res-cheap", &ad_id, 2_000_000);
        cheeky.agreed_price = Amount::new(1_000_000, 6);

        assert_eq!(
            reservations.apply_request(SignedReservationRequest::sign(cheeky, &buyer)),
            Err(ReservationError::PriceDisagreement),
        );
    }

    /// Refused, not silently corrected. Substituting the node's own idea of
    /// the price would bind a taker to something they never signed — the
    /// same failure this field prevents, arrived at from the other side.
    #[test]
    fn a_disagreeing_price_leaves_the_liquidity_untouched() {
        let (advertisements, reservations, _merchant, ad_id) = setup();
        let before = advertisements.get(&ad_id).unwrap().available_liquidity;

        let buyer = Keypair::generate();
        let mut wrong = request(&buyer, "res-wrong", &ad_id, 2_000_000);
        wrong.agreed_price = Amount::new(999, 6);
        let _ = reservations.apply_request(SignedReservationRequest::sign(wrong, &buyer));

        assert_eq!(
            advertisements.get(&ad_id).unwrap().available_liquidity,
            before,
            "a refused reservation must not have reserved anything"
        );
    }

    #[test]
    fn a_valid_request_locks_liquidity_and_is_stored_as_escrow_locked() {
        let (advertisements, reservations, _merchant, ad_id) = setup();
        let buyer = Keypair::generate();
        let id = reservations
            .apply_request(SignedReservationRequest::sign(
                request(&buyer, "res-1", &ad_id, 2_000_000),
                &buyer,
            ))
            .unwrap();

        let reservation = reservations.get(&id).unwrap();
        assert_eq!(reservation.state, ReservationState::EscrowLocked);
        assert_eq!(
            advertisements.get(&ad_id).unwrap().available_liquidity,
            Amount::new(8_000_000, 6)
        );
    }

    #[test]
    fn a_request_exceeding_remaining_liquidity_is_rejected() {
        // min=1M, max=5M, initial=6M: a first request leaves exactly 1M
        // remaining — enough to auto-disable neither yet — so a second
        // request at the minimum size still has too little to draw
        // against.
        let advertisements = Rc::new(AdvertisementRegistry::new(MemoryStore::new()));
        let merchant = Keypair::generate();
        let ad_id = AdvertisementId::new("ad-1");
        let create = AdvertisementCreate {
            id: ad_id.clone(),
            merchant: peer_id_from_public_key(&merchant.public_key()).unwrap(),
            merchant_public_key: merchant.public_key(),
            asset_mint: MintAddress::parse("2bHPi5hA4zrmPAfrvLmEexg3KJjpTjNkUcxWnzUPeRRU").unwrap(),
            direction: Direction::Sell,
            fiat_currency: FiatCurrency::parse("KES").unwrap(),
            min_trade: Amount::new(1_000_000, 6),
            max_trade: Amount::new(5_000_000, 6),
            initial_liquidity: Amount::new(6_000_000, 6),
            pricing: PricingModel::Fixed {
                price: Amount::new(129_000_000, 6),
            },
            payment_methods: vec!["Mobile Money".to_string()],
            timestamp: Timestamp::now(),
        };
        advertisements
            .apply_create(SignedAdvertisementCreate::sign(create, &merchant))
            .unwrap();
        let reservations = ReservationRegistry::new(MemoryStore::new(), Rc::clone(&advertisements));

        let buyer_a = Keypair::generate();
        let buyer_b = Keypair::generate();
        reservations
            .apply_request(SignedReservationRequest::sign(
                request(&buyer_a, "res-1", &ad_id, 5_000_000),
                &buyer_a,
            ))
            .unwrap();
        assert_eq!(
            advertisements.get(&ad_id).unwrap().available_liquidity,
            Amount::new(1_000_000, 6)
        );

        let result = reservations.apply_request(SignedReservationRequest::sign(
            request(&buyer_b, "res-2", &ad_id, 2_000_000),
            &buyer_b,
        ));
        assert_eq!(result, Err(ReservationError::InsufficientLiquidity));
    }

    #[test]
    fn an_amount_outside_the_advertisements_limits_is_rejected() {
        let (_advertisements, reservations, _merchant, ad_id) = setup();
        let buyer = Keypair::generate();
        let result = reservations.apply_request(SignedReservationRequest::sign(
            request(&buyer, "res-1", &ad_id, 100),
            &buyer,
        ));
        assert_eq!(result, Err(ReservationError::InvalidAmount));
    }

    #[test]
    fn cancelling_releases_liquidity_back_to_the_advertisement() {
        let (advertisements, reservations, _merchant, ad_id) = setup();
        let buyer = Keypair::generate();
        let id = reservations
            .apply_request(SignedReservationRequest::sign(
                request(&buyer, "res-1", &ad_id, 2_000_000),
                &buyer,
            ))
            .unwrap();

        let cancel = ReservationCancel {
            id: id.clone(),
            requester: peer_id_from_public_key(&buyer.public_key()).unwrap(),
            timestamp: Timestamp::now(),
        };
        reservations
            .apply_cancel(SignedReservationCancel::sign(cancel, &buyer))
            .unwrap();

        assert_eq!(
            reservations.get(&id).unwrap().state,
            ReservationState::Cancelled
        );
        assert_eq!(
            advertisements.get(&ad_id).unwrap().available_liquidity,
            Amount::new(10_000_000, 6)
        );
    }

    #[test]
    fn cancelling_from_a_different_wallet_is_rejected() {
        let (_advertisements, reservations, _merchant, ad_id) = setup();
        let buyer = Keypair::generate();
        let attacker = Keypair::generate();
        let id = reservations
            .apply_request(SignedReservationRequest::sign(
                request(&buyer, "res-1", &ad_id, 2_000_000),
                &buyer,
            ))
            .unwrap();

        let cancel = ReservationCancel {
            id,
            requester: peer_id_from_public_key(&buyer.public_key()).unwrap(),
            timestamp: Timestamp::now(),
        };
        let result = reservations.apply_cancel(SignedReservationCancel::sign(cancel, &attacker));
        assert_eq!(result, Err(ReservationError::InvalidSignature));
    }

    /// The reservation equivalent of signing your own permission slip.
    ///
    /// Every deadline this crate enforces is derived from a timestamp the
    /// requester chose, so without a bound a taker simply dates their
    /// request past the heat death of the merchant's patience and the
    /// sweep never touches it.
    #[test]
    fn a_requester_cannot_date_their_request_into_the_future_to_extend_their_own_window() {
        let (advertisements, reservations, _merchant, ad_id) = setup();
        let buyer = Keypair::generate();
        let mut req = request(&buyer, "res-far-future", &ad_id, 2_000_000);
        req.timestamp = Timestamp::from_millis(
            Timestamp::now().as_millis() + 365 * 24 * 60 * 60 * 1_000, // a year out
        );
        let liquidity_before = advertisements.get(&ad_id).unwrap().available_liquidity;

        assert_eq!(
            reservations.apply_request(SignedReservationRequest::sign(req, &buyer)),
            Err(ReservationError::TimestampTooFarAhead)
        );
        assert_eq!(
            advertisements.get(&ad_id).unwrap().available_liquidity,
            liquidity_before,
            "a refused request still moved the merchant's liquidity"
        );
    }

    /// The bound has to clear ordinary clock disagreement between honest,
    /// unsynchronised machines, or it becomes an outage for anyone whose
    /// laptop runs a few seconds fast.
    #[test]
    fn a_request_from_a_slightly_fast_clock_is_still_accepted() {
        let (_advertisements, reservations, _merchant, ad_id) = setup();
        let buyer = Keypair::generate();
        let mut req = request(&buyer, "res-fast-clock", &ad_id, 2_000_000);
        req.timestamp = Timestamp::from_millis(
            Timestamp::now().as_millis() + (protocol::MAX_CLOCK_SKEW.as_millis() as u64 / 2),
        );

        assert!(
            reservations
                .apply_request(SignedReservationRequest::sign(req, &buyer))
                .is_ok()
        );
    }

    #[test]
    fn expire_stale_releases_liquidity_for_reservations_past_the_window() {
        let (advertisements, reservations, _merchant, ad_id) = setup();
        let buyer = Keypair::generate();
        let mut req = request(&buyer, "res-1", &ad_id, 2_000_000);
        req.timestamp = Timestamp::from_millis(0);
        let id = reservations
            .apply_request(SignedReservationRequest::sign(req, &buyer))
            .unwrap();

        let sweep = reservations.expire_stale(Duration::from_secs(60));
        assert_eq!(
            sweep,
            ExpirySweep {
                expired: 1,
                deferred: 0
            }
        );
        assert_eq!(
            reservations.get(&id).unwrap().state,
            ReservationState::Expired
        );
        assert_eq!(
            advertisements.get(&ad_id).unwrap().available_liquidity,
            Amount::new(10_000_000, 6)
        );
    }

    /// The failure mode that would make wiring the sweep worse than
    /// leaving it off: marking a reservation `Expired` when the liquidity
    /// it was holding did not actually come back. The merchant's inventory
    /// would simply be gone, with a record saying the reservation was
    /// unwound cleanly.
    ///
    /// Nothing in the node deletes an advertisement today, so this state
    /// is reached here by removing the record underneath the registry —
    /// which is exactly what a future retention policy, a partially
    /// restored snapshot, or a corrupt value would look like from inside
    /// this function.
    #[test]
    fn a_reservation_whose_liquidity_cannot_be_returned_stays_locked_for_the_next_sweep() {
        let store = Rc::new(MemoryStore::new());
        let advertisements = Rc::new(AdvertisementRegistry::new(Rc::clone(&store)));
        let merchant = Keypair::generate();
        let ad_id = AdvertisementId::new("ad-vanishing");
        let create = AdvertisementCreate {
            id: ad_id.clone(),
            merchant: peer_id_from_public_key(&merchant.public_key()).unwrap(),
            merchant_public_key: merchant.public_key(),
            asset_mint: MintAddress::parse("2bHPi5hA4zrmPAfrvLmEexg3KJjpTjNkUcxWnzUPeRRU").unwrap(),
            direction: Direction::Sell,
            fiat_currency: FiatCurrency::parse("KES").unwrap(),
            min_trade: Amount::new(1_000_000, 6),
            max_trade: Amount::new(5_000_000, 6),
            initial_liquidity: Amount::new(10_000_000, 6),
            pricing: PricingModel::Fixed {
                price: Amount::new(129_000_000, 6),
            },
            payment_methods: vec!["Mobile Money".to_string()],
            timestamp: Timestamp::now(),
        };
        advertisements
            .apply_create(SignedAdvertisementCreate::sign(create, &merchant))
            .unwrap();
        let reservations =
            ReservationRegistry::new(Rc::new(MemoryStore::new()), Rc::clone(&advertisements));

        let buyer = Keypair::generate();
        let mut req = request(&buyer, "res-orphan", &ad_id, 2_000_000);
        req.timestamp = Timestamp::from_millis(0);
        let id = reservations
            .apply_request(SignedReservationRequest::sign(req, &buyer))
            .unwrap();

        store
            .delete("advertisements", ad_id.as_str().as_bytes())
            .unwrap();

        let sweep = reservations.expire_stale(Duration::from_secs(60));
        assert_eq!(
            sweep,
            ExpirySweep {
                expired: 0,
                deferred: 1
            }
        );
        assert_eq!(
            reservations.get(&id).unwrap().state,
            ReservationState::EscrowLocked,
            "a reservation the sweep could not unwind must stay locked, so the next sweep \
             retries it once the advertisement is back"
        );
    }
}
