//! The replicated local settlement index (§18: "settlement survives node
//! restarts... no completed work is lost" — the same replicated-KvStore
//! shape as every other store in this workspace provides that for free).

use crate::error::SettlementError;
use crate::events::{
    SignedPaymentReversed, SignedPaymentSubmitted, SignedSettlementApproved,
    SignedSettlementCancelled, SignedSettlementInitiate, SignedSettlementRejected,
};
use crate::protocol;
use crate::record::{Settlement, SettlementId, SettlementState};
use openfiat_crypto::verify;
use openfiat_serialization::json;
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{EventEnvelope, Timestamp};

const COLUMN_FAMILY: &str = "settlements";

pub struct SettlementRegistry<S> {
    store: S,
}

impl<S: KvStore> SettlementRegistry<S> {
    pub fn new(store: S) -> Self {
        Self { store }
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
    use openfiat_crypto::Keypair;
    use openfiat_network::identity::peer_id_from_public_key;
    use openfiat_reservations::ReservationId;
    use openfiat_storage::mem::MemoryStore;
    use openfiat_types::{Amount, Timestamp};

    fn setup() -> (
        SettlementRegistry<MemoryStore>,
        Keypair,
        Keypair,
        SettlementId,
    ) {
        let registry = SettlementRegistry::new(MemoryStore::new());
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
