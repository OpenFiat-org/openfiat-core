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
    pub fn expire_stale(&self, window: Duration) -> usize {
        let cutoff = Timestamp::now()
            .as_millis()
            .saturating_sub(window.as_millis() as u64);
        let mut expired = 0;
        for mut reservation in self.all() {
            if reservation.state == ReservationState::EscrowLocked
                && reservation.requested_at.as_millis() < cutoff
            {
                let _ = self
                    .advertisements
                    .release_liquidity(&reservation.advertisement_id, reservation.amount);
                reservation.state = ReservationState::Expired;
                reservation.updated_at = Timestamp::now();
                self.put(&reservation);
                expired += 1;
            }
        }
        expired
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
            fiat_currency: "KES".to_string(),
            min_trade: Amount::new(1_000_000, 6),
            max_trade: Amount::new(5_000_000, 6),
            initial_liquidity: Amount::new(10_000_000, 6),
            pricing: PricingModel::Fixed {
                price: Amount::new(129_000_000, 6),
            },
            payment_methods: vec![],
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
            timestamp: Timestamp::now(),
        }
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
            fiat_currency: "KES".to_string(),
            min_trade: Amount::new(1_000_000, 6),
            max_trade: Amount::new(5_000_000, 6),
            initial_liquidity: Amount::new(6_000_000, 6),
            pricing: PricingModel::Fixed {
                price: Amount::new(129_000_000, 6),
            },
            payment_methods: vec![],
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

    #[test]
    fn expire_stale_releases_liquidity_for_reservations_past_the_window() {
        let (advertisements, reservations, _merchant, ad_id) = setup();
        let buyer = Keypair::generate();
        let mut req = request(&buyer, "res-1", &ad_id, 2_000_000);
        req.timestamp = Timestamp::from_millis(0);
        let id = reservations
            .apply_request(SignedReservationRequest::sign(req, &buyer))
            .unwrap();

        let expired = reservations.expire_stale(Duration::from_secs(60));
        assert_eq!(expired, 1);
        assert_eq!(
            reservations.get(&id).unwrap().state,
            ReservationState::Expired
        );
        assert_eq!(
            advertisements.get(&ad_id).unwrap().available_liquidity,
            Amount::new(10_000_000, 6)
        );
    }
}
