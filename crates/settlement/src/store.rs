//! The replicated local settlement index (§18: "settlement survives node
//! restarts... no completed work is lost" — the same replicated-KvStore
//! shape as every other store in this workspace provides that for free),
//! plus a shared handle to the reservation registry whose reservations it
//! settles.
//!
//! That handle is the whole of §5a. A settlement and the reservation it
//! was raised against are two independently replicated records, and while
//! nothing connected them the reservation stayed in `EscrowLocked` for
//! the entire life of the trade — so the taker could cancel it mid-trade,
//! and the expiry sweep unwound it on its own after thirty minutes,
//! either way handing the merchant back liquidity that was committed to a
//! settlement still running. It is held here, and written from inside
//! this crate's own apply path, for two reasons: this crate already
//! depends on `openfiat-reservations` (for `ReservationId`), so nothing
//! new is introduced and no cycle is closed; and every write happens
//! inside the same deterministic function every replica runs, so the two
//! records move together on every node rather than only on whichever one
//! a client happened to talk to.

use crate::error::SettlementError;
use crate::events::{
    SignedPaymentReversed, SignedPaymentSubmitted, SignedSettlementApproved,
    SignedSettlementCancelled, SignedSettlementInitiate, SignedSettlementRejected,
};
use crate::protocol;
use crate::record::{DisputeVerdict, Settlement, SettlementId, SettlementState};
use openfiat_crypto::verify;
use openfiat_reservations::ReservationRegistry;
use openfiat_serialization::json;
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{EventEnvelope, Timestamp};
use std::rc::Rc;

const COLUMN_FAMILY: &str = "settlements";

pub struct SettlementRegistry<S> {
    store: S,
    reservations: Rc<ReservationRegistry<S>>,
}

impl<S: KvStore> SettlementRegistry<S> {
    pub fn new(store: S, reservations: Rc<ReservationRegistry<S>>) -> Self {
        Self {
            store,
            reservations,
        }
    }

    pub fn get(&self, id: &SettlementId) -> Option<Settlement> {
        let bytes = self
            .store
            .get(COLUMN_FAMILY, id.as_str().as_bytes())
            .ok()
            .flatten()?;
        wire::from_bytes(&bytes).ok()
    }

    fn put(&self, settlement: &Settlement) {
        if let Ok(bytes) = wire::to_bytes(settlement) {
            let _ = self
                .store
                .put(COLUMN_FAMILY, settlement.id.as_str().as_bytes(), &bytes);
        }
    }

    pub fn all(&self) -> Vec<Settlement> {
        self.store
            .iter_prefix(COLUMN_FAMILY, &[])
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(_, value)| wire::from_bytes(&value).ok())
            .collect()
    }

    pub fn apply_initiate(
        &self,
        signed: SignedSettlementInitiate,
    ) -> Result<SettlementId, SettlementError> {
        signed.verify()?;
        let id = signed.initiate.id.clone();
        if self.get(&id).is_some() {
            return Err(SettlementError::DuplicateSettlementId);
        }
        let initiate = signed.initiate;
        // §5a: the reservation is now committed to this trade, so neither
        // its owner nor the expiry sweep may hand its liquidity back while
        // the settlement runs. Best-effort by design — see
        // `ReservationRegistry::settlement_started` for why a node that
        // does not hold the reservation, or holds it in another state,
        // must still accept the settlement rather than drop a live trade.
        let _ = self
            .reservations
            .settlement_started(&initiate.reservation_id);
        self.put(&Settlement {
            id: id.clone(),
            reservation_id: initiate.reservation_id,
            buyer: initiate.buyer,
            buyer_public_key: initiate.buyer_public_key,
            seller: initiate.seller,
            seller_public_key: initiate.seller_public_key,
            amount: initiate.amount,
            state: SettlementState::AwaitingPayment,
            payment_reference: None,
            escrow_release_signature: None,
            payment_submitted_at: None,
            merchant_responded_at: None,
            payment_discrepancy: None,
            disputed_at: None,
            created_at: initiate.timestamp,
            updated_at: initiate.timestamp,
        });
        Ok(id)
    }

    /// §9: the buyer's "I Paid" declaration. Only legal from
    /// `AwaitingPayment`.
    pub fn apply_payment_submitted(
        &self,
        signed: SignedPaymentSubmitted,
    ) -> Result<(), SettlementError> {
        let mut settlement = self
            .get(&signed.action.settlement_id)
            .ok_or(SettlementError::SettlementNotFound)?;
        if settlement.buyer != signed.action.buyer {
            return Err(SettlementError::Unauthorized);
        }
        let bytes =
            json::to_bytes(&signed.action).map_err(|_| SettlementError::MalformedSettlement)?;
        verify(&settlement.buyer_public_key, &bytes, &signed.signature)
            .map_err(|_| SettlementError::InvalidSignature)?;
        if settlement.state != SettlementState::AwaitingPayment {
            return Err(SettlementError::InvalidStateTransition);
        }

        settlement.state = SettlementState::PaymentSubmitted;
        settlement.payment_reference = signed.action.payment_reference;
        settlement.payment_submitted_at = Some(signed.action.timestamp);
        settlement.updated_at = signed.action.timestamp;
        self.put(&settlement);
        Ok(())
    }

    /// §10: withdraw "I Paid" — only legal before approval.
    pub fn apply_payment_reversed(
        &self,
        signed: SignedPaymentReversed,
    ) -> Result<(), SettlementError> {
        let mut settlement = self
            .get(&signed.action.settlement_id)
            .ok_or(SettlementError::SettlementNotFound)?;
        if settlement.buyer != signed.action.buyer {
            return Err(SettlementError::Unauthorized);
        }
        let bytes =
            json::to_bytes(&signed.action).map_err(|_| SettlementError::MalformedSettlement)?;
        verify(&settlement.buyer_public_key, &bytes, &signed.signature)
            .map_err(|_| SettlementError::InvalidSignature)?;
        if settlement.state != SettlementState::PaymentSubmitted {
            return Err(SettlementError::InvalidStateTransition);
        }

        settlement.state = SettlementState::AwaitingPayment;
        settlement.payment_reference = None;
        // The declaration is withdrawn, so there is no outstanding one
        // for the merchant to be judged on responding to.
        settlement.payment_submitted_at = None;
        settlement.updated_at = signed.action.timestamp;
        self.put(&settlement);
        Ok(())
    }

    /// §15-16: the merchant's approval — legal only from
    /// `PaymentSubmitted`. This crate's authority ends here in the sense
    /// that it never itself constructs or submits the on-chain
    /// `release_escrow` instruction (that's the seller's own wallet,
    /// client-side, via OFS-4300); `Approved` is the real, held state
    /// until [`Self::apply_escrow_released`] observes that confirmed.
    pub fn apply_approved(&self, signed: SignedSettlementApproved) -> Result<(), SettlementError> {
        let mut settlement = self
            .get(&signed.action.settlement_id)
            .ok_or(SettlementError::SettlementNotFound)?;
        if settlement.seller != signed.action.seller {
            return Err(SettlementError::Unauthorized);
        }
        let bytes =
            json::to_bytes(&signed.action).map_err(|_| SettlementError::MalformedSettlement)?;
        verify(&settlement.seller_public_key, &bytes, &signed.signature)
            .map_err(|_| SettlementError::InvalidSignature)?;
        if settlement.state != SettlementState::PaymentSubmitted {
            return Err(SettlementError::InvalidStateTransition);
        }

        settlement.state = SettlementState::Approved;
        settlement.merchant_responded_at = Some(signed.action.timestamp);
        settlement.updated_at = signed.action.timestamp;
        // §5a: the trade is done as far as the reservation is concerned —
        // the asset is on its way to the buyer, so its liquidity must not
        // find its way back to the advertisement. Recorded at `Approved`
        // rather than at `Completed` because `Completed` is a node-local
        // observation of the chain (see `apply_escrow_released`) and two
        // honest nodes reach it at different times; `Approved` is a signed
        // event every replica applies.
        let _ = self
            .reservations
            .settlement_completed(&settlement.reservation_id);
        self.put(&settlement);
        Ok(())
    }

    /// Records that the on-chain `release_escrow` transaction has been
    /// independently observed as confirmed (OFS-4300 §7-8) — purely
    /// local bookkeeping, like `openfiat-reservations::expire_stale`,
    /// not a new signed peer-to-peer event: on-chain confirmation is
    /// equally verifiable by every node, not something one peer asserts
    /// to another. Legal only from `Approved`.
    pub fn apply_escrow_released(
        &self,
        id: &SettlementId,
        signature: impl Into<String>,
    ) -> Result<(), SettlementError> {
        let mut settlement = self.get(id).ok_or(SettlementError::SettlementNotFound)?;
        if settlement.state != SettlementState::Approved {
            return Err(SettlementError::InvalidStateTransition);
        }

        settlement.state = SettlementState::Completed;
        settlement.escrow_release_signature = Some(signature.into());
        settlement.updated_at = Timestamp::now();
        self.put(&settlement);
        Ok(())
    }

    pub fn apply_rejected(&self, signed: SignedSettlementRejected) -> Result<(), SettlementError> {
        let mut settlement = self
            .get(&signed.action.settlement_id)
            .ok_or(SettlementError::SettlementNotFound)?;
        if settlement.seller != signed.action.seller {
            return Err(SettlementError::Unauthorized);
        }
        let bytes =
            json::to_bytes(&signed.action).map_err(|_| SettlementError::MalformedSettlement)?;
        verify(&settlement.seller_public_key, &bytes, &signed.signature)
            .map_err(|_| SettlementError::InvalidSignature)?;
        if settlement.state != SettlementState::PaymentSubmitted {
            return Err(SettlementError::InvalidStateTransition);
        }

        settlement.state = SettlementState::Rejected;
        settlement.merchant_responded_at = Some(signed.action.timestamp);
        settlement.payment_discrepancy = Some(signed.action.discrepancy);
        settlement.updated_at = signed.action.timestamp;
        // §5a: no transfer happened, so the reservation goes back to being
        // an ordinary locked one — cancellable again, and swept if nobody
        // does anything with it.
        let _ = self
            .reservations
            .settlement_abandoned(&settlement.reservation_id);
        self.put(&settlement);
        Ok(())
    }

    /// §19/§19a: either party may cancel, but only before payment is
    /// marked sent.
    pub fn apply_cancelled(
        &self,
        signed: SignedSettlementCancelled,
    ) -> Result<(), SettlementError> {
        let mut settlement = self
            .get(&signed.action.settlement_id)
            .ok_or(SettlementError::SettlementNotFound)?;
        let canceller_key = if signed.action.canceller == settlement.buyer {
            settlement.buyer_public_key
        } else if signed.action.canceller == settlement.seller {
            settlement.seller_public_key
        } else {
            return Err(SettlementError::Unauthorized);
        };
        let bytes =
            json::to_bytes(&signed.action).map_err(|_| SettlementError::MalformedSettlement)?;
        verify(&canceller_key, &bytes, &signed.signature)
            .map_err(|_| SettlementError::InvalidSignature)?;
        if settlement.state != SettlementState::AwaitingPayment {
            return Err(SettlementError::InvalidStateTransition);
        }

        settlement.state = SettlementState::Cancelled;
        settlement.updated_at = signed.action.timestamp;
        // §5a: as for a rejection — nothing moved, so the reservation is
        // live again for whatever is left of its own window.
        let _ = self
            .reservations
            .settlement_abandoned(&settlement.reservation_id);
        self.put(&settlement);
        Ok(())
    }

    /// OFS-2400 §5-6: this settlement has been escalated to arbitration
    /// and its escrow is frozen.
    ///
    /// Called by `openfiat-disputes` from inside `apply_open`, so a node
    /// that accepts the dispute event records the freeze and one that
    /// refuses it records nothing — the two never come apart.
    ///
    /// Legal from every state except `Cancelled` (no escrow was ever at
    /// stake) and `Disputed` (already frozen; a second case on one
    /// settlement is what OFS-2400 §5's "only one dispute may be open per
    /// settlement" forbids). `Approved` and `Completed` are deliberately
    /// *not* distinguished, here or anywhere: `Completed` is a node-local
    /// observation of the chain, so a rule that admitted one and refused
    /// the other would accept a dispute on one node and refuse it on its
    /// neighbour for the same settlement at the same instant.
    pub fn apply_dispute_opened(
        &self,
        id: &SettlementId,
        opened_at: Timestamp,
    ) -> Result<(), SettlementError> {
        let mut settlement = self.get(id).ok_or(SettlementError::SettlementNotFound)?;
        if matches!(
            settlement.state,
            SettlementState::Cancelled | SettlementState::Disputed
        ) {
            return Err(SettlementError::InvalidStateTransition);
        }

        settlement.state = SettlementState::Disputed;
        settlement.disputed_at = Some(opened_at);
        settlement.updated_at = opened_at;
        // The reservation is already `Settling` and stays there: a frozen
        // escrow is the strongest form of "this liquidity is committed"
        // there is, so nothing about a dispute should let the reservation
        // be cancelled or swept out from under it.
        self.put(&settlement);
        Ok(())
    }

    /// OFS-2400 §17: arbitration concluded and the chain moved the escrow.
    ///
    /// The exit `Disputed` has to have. Recorded from the same on-chain
    /// execution `openfiat-disputes` independently observed — local
    /// bookkeeping, like [`Self::apply_escrow_released`], because a
    /// confirmed transaction is equally verifiable by every node rather
    /// than something one peer asserts to another.
    ///
    /// The settlement lands in the state its outcome actually corresponds
    /// to, not back where it was escalated from: a released escrow is a
    /// completed trade whatever route it took, and a returned one is an
    /// abandoned trade. Restoring the pre-dispute state instead would put
    /// an arbitrated settlement back into `PaymentSubmitted` — live,
    /// awaiting a merchant decision that has already been made for them
    /// and will never come.
    pub fn apply_dispute_resolved(
        &self,
        id: &SettlementId,
        verdict: DisputeVerdict,
        execution_signature: &str,
    ) -> Result<(), SettlementError> {
        let mut settlement = self.get(id).ok_or(SettlementError::SettlementNotFound)?;
        if settlement.state != SettlementState::Disputed {
            return Err(SettlementError::InvalidStateTransition);
        }

        match verdict {
            DisputeVerdict::EscrowReleased => {
                settlement.state = SettlementState::Completed;
                // Only if the escrow had not already been released before
                // the dispute was opened. `execute_dispute_outcome` is the
                // transaction that moved the funds in every other case,
                // and overwriting a release signature that names a
                // different, earlier transaction would misreport where the
                // money actually went.
                settlement
                    .escrow_release_signature
                    .get_or_insert_with(|| execution_signature.to_string());
                let _ = self
                    .reservations
                    .settlement_completed(&settlement.reservation_id);
            }
            DisputeVerdict::EscrowReturned => {
                settlement.state = SettlementState::Cancelled;
                let _ = self
                    .reservations
                    .settlement_abandoned(&settlement.reservation_id);
            }
        }
        settlement.updated_at = Timestamp::now();
        self.put(&settlement);
        Ok(())
    }

    pub fn apply_event(&self, event: &EventEnvelope) {
        if event.ofs_spec != protocol::OFS_SPEC {
            return;
        }
        match event.event_type.as_str() {
            protocol::EVENT_INITIATED => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_initiate(signed);
                }
            }
            protocol::EVENT_PAYMENT_SUBMITTED => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_payment_submitted(signed);
                }
            }
            protocol::EVENT_PAYMENT_REVERSED => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_payment_reversed(signed);
                }
            }
            protocol::EVENT_APPROVED => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_approved(signed);
                }
            }
            protocol::EVENT_REJECTED => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_rejected(signed);
                }
            }
            protocol::EVENT_CANCELLED => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_cancelled(signed);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{
        PaymentReversed, PaymentSubmitted, SettlementApproved, SettlementCancelled,
        SettlementInitiate, SettlementRejected,
    };
    use crate::record::PaymentDiscrepancy;
    use openfiat_advertisements::AdvertisementRegistry;
    use openfiat_crypto::Keypair;
    use openfiat_network::identity::peer_id_from_public_key;
    use openfiat_reservations::{ReservationId, ReservationRegistry};
    use openfiat_storage::mem::MemoryStore;
    use openfiat_types::{Amount, Timestamp};

    /// The reservation index a settlement registry requires (OFS-2300
    /// §5a), with nothing in it. Tests that care about the reservation
    /// side seed it themselves.
    fn reservations() -> Rc<ReservationRegistry<MemoryStore>> {
        Rc::new(ReservationRegistry::new(
            MemoryStore::new(),
            Rc::new(AdvertisementRegistry::new(MemoryStore::new())),
        ))
    }

    fn setup() -> (
        SettlementRegistry<MemoryStore>,
        Keypair,
        Keypair,
        SettlementId,
    ) {
        let registry = SettlementRegistry::new(MemoryStore::new(), reservations());
        let buyer = Keypair::generate();
        let seller = Keypair::generate();
        let initiate = SettlementInitiate {
            id: SettlementId::new("settle-1"),
            reservation_id: ReservationId::new("res-1"),
            buyer: peer_id_from_public_key(&buyer.public_key()).unwrap(),
            buyer_public_key: buyer.public_key(),
            seller: peer_id_from_public_key(&seller.public_key()).unwrap(),
            seller_public_key: seller.public_key(),
            amount: Amount::new(2_000_000, 6),
            timestamp: Timestamp::now(),
        };
        let id = registry
            .apply_initiate(SignedSettlementInitiate::sign(initiate, &buyer))
            .unwrap();
        (registry, buyer, seller, id)
    }

    #[test]
    fn a_full_happy_path_reaches_completed() {
        let (registry, buyer, seller, id) = setup();
        let buyer_id = peer_id_from_public_key(&buyer.public_key()).unwrap();
        let seller_id = peer_id_from_public_key(&seller.public_key()).unwrap();

        registry
            .apply_payment_submitted(SignedPaymentSubmitted::sign(
                PaymentSubmitted {
                    settlement_id: id.clone(),
                    buyer: buyer_id,
                    payment_reference: Some("TXN123".to_string()),
                    timestamp: Timestamp::now(),
                },
                &buyer,
            ))
            .unwrap();
        assert_eq!(
            registry.get(&id).unwrap().state,
            SettlementState::PaymentSubmitted
        );

        registry
            .apply_approved(SignedSettlementApproved::sign(
                SettlementApproved {
                    settlement_id: id.clone(),
                    seller: seller_id,
                    timestamp: Timestamp::now(),
                },
                &seller,
            ))
            .unwrap();
        assert_eq!(registry.get(&id).unwrap().state, SettlementState::Approved);
        assert_eq!(registry.get(&id).unwrap().escrow_release_signature, None);

        registry
            .apply_escrow_released(&id, "5xY...onchainSig")
            .unwrap();
        let settlement = registry.get(&id).unwrap();
        assert_eq!(settlement.state, SettlementState::Completed);
        assert_eq!(
            settlement.escrow_release_signature,
            Some("5xY...onchainSig".to_string())
        );
    }

    #[test]
    fn escrow_release_cannot_be_recorded_before_approval() {
        let (registry, buyer, _seller, id) = setup();
        let buyer_id = peer_id_from_public_key(&buyer.public_key()).unwrap();
        registry
            .apply_payment_submitted(SignedPaymentSubmitted::sign(
                PaymentSubmitted {
                    settlement_id: id.clone(),
                    buyer: buyer_id,
                    payment_reference: None,
                    timestamp: Timestamp::now(),
                },
                &buyer,
            ))
            .unwrap();

        let result = registry.apply_escrow_released(&id, "sig");
        assert_eq!(result, Err(SettlementError::InvalidStateTransition));
    }

    #[test]
    fn the_buyer_cannot_approve_their_own_payment() {
        let (registry, buyer, _seller, id) = setup();
        let buyer_id = peer_id_from_public_key(&buyer.public_key()).unwrap();
        registry
            .apply_payment_submitted(SignedPaymentSubmitted::sign(
                PaymentSubmitted {
                    settlement_id: id.clone(),
                    buyer: buyer_id.clone(),
                    payment_reference: None,
                    timestamp: Timestamp::now(),
                },
                &buyer,
            ))
            .unwrap();

        let result = registry.apply_approved(SignedSettlementApproved::sign(
            SettlementApproved {
                settlement_id: id,
                seller: buyer_id,
                timestamp: Timestamp::now(),
            },
            &buyer,
        ));
        assert_eq!(result, Err(SettlementError::Unauthorized));
    }

    #[test]
    fn payment_can_be_reversed_before_approval() {
        let (registry, buyer, _seller, id) = setup();
        let buyer_id = peer_id_from_public_key(&buyer.public_key()).unwrap();
        registry
            .apply_payment_submitted(SignedPaymentSubmitted::sign(
                PaymentSubmitted {
                    settlement_id: id.clone(),
                    buyer: buyer_id.clone(),
                    payment_reference: None,
                    timestamp: Timestamp::now(),
                },
                &buyer,
            ))
            .unwrap();
        registry
            .apply_payment_reversed(SignedPaymentReversed::sign(
                PaymentReversed {
                    settlement_id: id.clone(),
                    buyer: buyer_id,
                    timestamp: Timestamp::now(),
                },
                &buyer,
            ))
            .unwrap();
        assert_eq!(
            registry.get(&id).unwrap().state,
            SettlementState::AwaitingPayment
        );
    }

    #[test]
    fn rejection_moves_to_rejected() {
        let (registry, buyer, seller, id) = setup();
        let buyer_id = peer_id_from_public_key(&buyer.public_key()).unwrap();
        let seller_id = peer_id_from_public_key(&seller.public_key()).unwrap();
        registry
            .apply_payment_submitted(SignedPaymentSubmitted::sign(
                PaymentSubmitted {
                    settlement_id: id.clone(),
                    buyer: buyer_id,
                    payment_reference: None,
                    timestamp: Timestamp::now(),
                },
                &buyer,
            ))
            .unwrap();
        registry
            .apply_rejected(SignedSettlementRejected::sign(
                SettlementRejected {
                    settlement_id: id.clone(),
                    seller: seller_id,
                    reason: "no matching deposit".to_string(),
                    discrepancy: PaymentDiscrepancy::IncorrectAmount,
                    timestamp: Timestamp::now(),
                },
                &seller,
            ))
            .unwrap();
        assert_eq!(registry.get(&id).unwrap().state, SettlementState::Rejected);
    }

    #[test]
    fn cancellation_is_rejected_once_payment_has_been_submitted() {
        let (registry, buyer, _seller, id) = setup();
        let buyer_id = peer_id_from_public_key(&buyer.public_key()).unwrap();
        registry
            .apply_payment_submitted(SignedPaymentSubmitted::sign(
                PaymentSubmitted {
                    settlement_id: id.clone(),
                    buyer: buyer_id.clone(),
                    payment_reference: None,
                    timestamp: Timestamp::now(),
                },
                &buyer,
            ))
            .unwrap();

        let result = registry.apply_cancelled(SignedSettlementCancelled::sign(
            SettlementCancelled {
                settlement_id: id,
                canceller: buyer_id,
                timestamp: Timestamp::now(),
            },
            &buyer,
        ));
        assert_eq!(result, Err(SettlementError::InvalidStateTransition));
    }

    /// A settlement raised against a real reservation, so the §5a
    /// transitions have something to move. Everything above uses a bare
    /// `ReservationId` with no record behind it, which is also a real
    /// case (a node that has the settlement and not the reservation) and
    /// is why those tests still pass unchanged.
    fn over_a_reservation() -> (
        Rc<ReservationRegistry<MemoryStore>>,
        SettlementRegistry<MemoryStore>,
        Keypair,
        Keypair,
        ReservationId,
        SettlementId,
    ) {
        use openfiat_advertisements::events::{AdvertisementCreate, SignedAdvertisementCreate};
        use openfiat_advertisements::{AdvertisementId, Direction, PricingModel};
        use openfiat_reservations::events::{ReservationRequest, SignedReservationRequest};

        let advertisements = Rc::new(AdvertisementRegistry::new(MemoryStore::new()));
        let seller = Keypair::generate();
        let buyer = Keypair::generate();
        let ad_id = AdvertisementId::new("ad-1");
        advertisements
            .apply_create(SignedAdvertisementCreate::sign(
                AdvertisementCreate {
                    id: ad_id.clone(),
                    merchant: peer_id_from_public_key(&seller.public_key()).unwrap(),
                    merchant_public_key: seller.public_key(),
                    asset_mint: openfiat_crypto::MintAddress::parse(
                        "2bHPi5hA4zrmPAfrvLmEexg3KJjpTjNkUcxWnzUPeRRU",
                    )
                    .unwrap(),
                    direction: Direction::Sell,
                    fiat_currency: openfiat_types::FiatCurrency::parse("KES").unwrap(),
                    min_trade: Amount::new(1_000_000, 6),
                    max_trade: Amount::new(5_000_000, 6),
                    initial_liquidity: Amount::new(10_000_000, 6),
                    pricing: PricingModel::Fixed {
                        price: Amount::new(129_000_000, 6),
                    },
                    payment_methods: vec![
                        openfiat_taxonomy::PaymentMethodRef::builtin("mpesa-kenya").unwrap(),
                    ],
                    timestamp: Timestamp::now(),
                },
                &seller,
            ))
            .unwrap();
        let reservations = Rc::new(ReservationRegistry::new(
            MemoryStore::new(),
            Rc::clone(&advertisements),
        ));
        let reservation_id = reservations
            .apply_request(SignedReservationRequest::sign(
                ReservationRequest {
                    id: ReservationId::new("res-real"),
                    advertisement_id: ad_id,
                    requester: peer_id_from_public_key(&buyer.public_key()).unwrap(),
                    requester_public_key: buyer.public_key(),
                    amount: Amount::new(2_000_000, 6),
                    agreed_price: Amount::new(129_000_000, 6),
                    agreed_mid: None,
                    timestamp: Timestamp::now(),
                },
                &buyer,
            ))
            .unwrap();

        let registry = SettlementRegistry::new(MemoryStore::new(), Rc::clone(&reservations));
        let id = registry
            .apply_initiate(SignedSettlementInitiate::sign(
                SettlementInitiate {
                    id: SettlementId::new("settle-real"),
                    reservation_id: reservation_id.clone(),
                    buyer: peer_id_from_public_key(&buyer.public_key()).unwrap(),
                    buyer_public_key: buyer.public_key(),
                    seller: peer_id_from_public_key(&seller.public_key()).unwrap(),
                    seller_public_key: seller.public_key(),
                    amount: Amount::new(2_000_000, 6),
                    timestamp: Timestamp::now(),
                },
                &buyer,
            ))
            .unwrap();
        (reservations, registry, buyer, seller, reservation_id, id)
    }

    fn state_of(
        reservations: &ReservationRegistry<MemoryStore>,
        id: &ReservationId,
    ) -> openfiat_reservations::ReservationState {
        reservations.get(id).expect("the reservation exists").state
    }

    /// §5a: raising a settlement is what commits the reservation. Until
    /// this, initiating a settlement left the reservation in
    /// `EscrowLocked` — cancellable by its owner and expirable by the
    /// sweep — for the whole life of the trade.
    #[test]
    fn initiating_a_settlement_commits_the_reservation_behind_it() {
        let (reservations, _registry, _buyer, _seller, reservation_id, _id) = over_a_reservation();
        assert_eq!(
            state_of(&reservations, &reservation_id),
            openfiat_reservations::ReservationState::Settling
        );
    }

    #[test]
    fn approval_settles_the_reservation_and_rejection_releases_it() {
        use openfiat_reservations::ReservationState;

        let (reservations, registry, buyer, seller, reservation_id, id) = over_a_reservation();
        let buyer_id = peer_id_from_public_key(&buyer.public_key()).unwrap();
        let seller_id = peer_id_from_public_key(&seller.public_key()).unwrap();
        let submit = |at: Timestamp| {
            registry
                .apply_payment_submitted(SignedPaymentSubmitted::sign(
                    PaymentSubmitted {
                        settlement_id: id.clone(),
                        buyer: buyer_id.clone(),
                        payment_reference: None,
                        timestamp: at,
                    },
                    &buyer,
                ))
                .unwrap();
        };

        submit(Timestamp::from_millis(1));
        registry
            .apply_rejected(SignedSettlementRejected::sign(
                SettlementRejected {
                    settlement_id: id.clone(),
                    seller: seller_id.clone(),
                    reason: "no matching deposit".to_string(),
                    discrepancy: PaymentDiscrepancy::IncorrectAmount,
                    timestamp: Timestamp::from_millis(2),
                },
                &seller,
            ))
            .unwrap();
        assert_eq!(
            state_of(&reservations, &reservation_id),
            ReservationState::EscrowLocked,
            "nothing was transferred, so the reservation is an ordinary locked one again"
        );

        // The same reservation, settled properly the second time round.
        let second = SettlementId::new("settle-real-2");
        registry
            .apply_initiate(SignedSettlementInitiate::sign(
                SettlementInitiate {
                    id: second.clone(),
                    reservation_id: reservation_id.clone(),
                    buyer: buyer_id.clone(),
                    buyer_public_key: buyer.public_key(),
                    seller: seller_id.clone(),
                    seller_public_key: seller.public_key(),
                    amount: Amount::new(2_000_000, 6),
                    timestamp: Timestamp::from_millis(3),
                },
                &buyer,
            ))
            .unwrap();
        registry
            .apply_payment_submitted(SignedPaymentSubmitted::sign(
                PaymentSubmitted {
                    settlement_id: second.clone(),
                    buyer: buyer_id,
                    payment_reference: None,
                    timestamp: Timestamp::from_millis(4),
                },
                &buyer,
            ))
            .unwrap();
        registry
            .apply_approved(SignedSettlementApproved::sign(
                SettlementApproved {
                    settlement_id: second,
                    seller: seller_id,
                    timestamp: Timestamp::from_millis(5),
                },
                &seller,
            ))
            .unwrap();
        assert_eq!(
            state_of(&reservations, &reservation_id),
            ReservationState::Settled,
            "the asset was sold, so the reservation's liquidity is spent, not returned"
        );
    }

    /// §5a's dispute entry and exit, without going through
    /// `openfiat-disputes` (which cannot be linked from here — it depends
    /// on this crate). The exit is the half that makes the entry safe:
    /// `apply_escrow_released` requires `Approved`, so a settlement left
    /// in `Disputed` could never record the release a buyer won.
    #[test]
    fn a_dispute_freezes_a_settlement_and_its_outcome_is_what_releases_it() {
        let (registry, buyer, _seller, id) = setup();
        let buyer_id = peer_id_from_public_key(&buyer.public_key()).unwrap();
        registry
            .apply_payment_submitted(SignedPaymentSubmitted::sign(
                PaymentSubmitted {
                    settlement_id: id.clone(),
                    buyer: buyer_id,
                    payment_reference: None,
                    timestamp: Timestamp::from_millis(1),
                },
                &buyer,
            ))
            .unwrap();

        registry
            .apply_dispute_opened(&id, Timestamp::from_millis(2))
            .expect("a live settlement can be escalated");
        let frozen = registry.get(&id).unwrap();
        assert_eq!(frozen.state, SettlementState::Disputed);
        assert_eq!(frozen.disputed_at, Some(Timestamp::from_millis(2)));
        assert_eq!(
            registry.apply_dispute_opened(&id, Timestamp::from_millis(3)),
            Err(SettlementError::InvalidStateTransition),
            "only one dispute may be open per settlement"
        );

        registry
            .apply_dispute_resolved(&id, DisputeVerdict::EscrowReleased, "arb-sig")
            .expect("the chain executed the case");
        let resolved = registry.get(&id).unwrap();
        assert_eq!(resolved.state, SettlementState::Completed);
        assert_eq!(
            resolved.escrow_release_signature,
            Some("arb-sig".to_string())
        );
        assert_eq!(
            resolved.disputed_at,
            Some(Timestamp::from_millis(2)),
            "that the trade was arbitrated survives the case closing"
        );
    }

    #[test]
    fn a_dispute_the_merchant_wins_ends_the_settlement_with_no_transfer() {
        let (registry, _buyer, _seller, id) = setup();
        registry
            .apply_dispute_opened(&id, Timestamp::from_millis(1))
            .unwrap();
        registry
            .apply_dispute_resolved(&id, DisputeVerdict::EscrowReturned, "arb-sig")
            .unwrap();

        let resolved = registry.get(&id).unwrap();
        assert_eq!(resolved.state, SettlementState::Cancelled);
        assert_eq!(
            resolved.escrow_release_signature, None,
            "nothing was released, so nothing names a release"
        );
    }

    /// A settlement nobody ever paid into and both parties walked away
    /// from has no escrow to freeze, so there is nothing to arbitrate.
    #[test]
    fn a_cancelled_settlement_cannot_be_disputed() {
        let (registry, buyer, _seller, id) = setup();
        let buyer_id = peer_id_from_public_key(&buyer.public_key()).unwrap();
        registry
            .apply_cancelled(SignedSettlementCancelled::sign(
                SettlementCancelled {
                    settlement_id: id.clone(),
                    canceller: buyer_id,
                    timestamp: Timestamp::now(),
                },
                &buyer,
            ))
            .unwrap();

        assert_eq!(
            registry.apply_dispute_opened(&id, Timestamp::now()),
            Err(SettlementError::InvalidStateTransition)
        );
    }

    #[test]
    fn cancellation_before_payment_is_allowed_by_either_party() {
        let (registry, _buyer, seller, id) = setup();
        let seller_id = peer_id_from_public_key(&seller.public_key()).unwrap();
        registry
            .apply_cancelled(SignedSettlementCancelled::sign(
                SettlementCancelled {
                    settlement_id: id.clone(),
                    canceller: seller_id,
                    timestamp: Timestamp::now(),
                },
                &seller,
            ))
            .unwrap();
        assert_eq!(registry.get(&id).unwrap().state, SettlementState::Cancelled);
    }
}
