//! Drives one node's settlement index: applies incoming gossip events
//! automatically and provides the operations that originate new ones.

use crate::error::SettlementError;
use crate::events::{
    PaymentReversed, PaymentSubmitted, SettlementApproved, SettlementCancelled, SettlementInitiate,
    SettlementRejected, SignedPaymentReversed, SignedPaymentSubmitted, SignedSettlementApproved,
    SignedSettlementCancelled, SignedSettlementInitiate, SignedSettlementRejected,
};
use crate::protocol;
use crate::record::{PaymentDiscrepancy, Settlement, SettlementId};
use crate::store::SettlementRegistry;
use openfiat_gossip::GossipService;
use openfiat_reservations::{ReservationId, ReservationRegistry};
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{Amount, EventType, PeerId, Priority, PublicKey, Timestamp};
use std::rc::Rc;

pub struct SettlementService<S> {
    pub gossip: GossipService<S>,
    registry: Rc<SettlementRegistry<S>>,
}

impl<S: KvStore + 'static> SettlementService<S> {
    /// `reservations` is this node's reservation index — the same handle
    /// `ReservationService::registry` hands out. Required rather than
    /// optional: a settlement registry that cannot mark a reservation
    /// `Settling` would silently allow the reservation to be cancelled or
    /// swept out from under a live trade (§5a), and an invariant that
    /// depends on remembering to wire something is not an invariant.
    pub fn new(
        mut gossip: GossipService<S>,
        store: S,
        reservations: Rc<ReservationRegistry<S>>,
    ) -> Self {
        let registry = Rc::new(SettlementRegistry::new(store, reservations));
        let handler_registry = Rc::clone(&registry);
        gossip.add_event_handler(move |event| handler_registry.apply_event(event));
        Self { gossip, registry }
    }

    /// A shared handle to this node's settlement index, for crates
    /// downstream in the dependency chain (`openfiat-trade`) that
    /// correlate settlements with their reservations rather than
    /// re-deriving settlement state themselves.
    pub fn registry(&self) -> Rc<SettlementRegistry<S>> {
        Rc::clone(&self.registry)
    }

    pub fn get(&self, id: &SettlementId) -> Option<Settlement> {
        self.registry.get(id)
    }

    pub fn all(&self) -> Vec<Settlement> {
        self.registry.all()
    }

    /// §1: begin tracking settlement for an already-locked reservation.
    /// Called by the buyer's own node — the buyer's successful
    /// reservation is what starts settlement.
    pub fn initiate(
        &mut self,
        id: impl Into<String>,
        reservation_id: ReservationId,
        seller: PeerId,
        seller_public_key: PublicKey,
        amount: Amount,
    ) -> Result<SettlementId, SettlementError> {
        let initiate = SettlementInitiate {
            id: SettlementId::new(id),
            reservation_id,
            buyer: self.gossip.node.local_peer_id(),
            buyer_public_key: self.gossip.public_key(),
            seller,
            seller_public_key,
            amount,
            timestamp: Timestamp::now(),
        };
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::SETTLEMENT_INITIATE,
            &initiate,
        )
        .expect("SettlementInitiate always serializes");
        let signed = SignedSettlementInitiate {
            signature: self.gossip.sign(&bytes),
            initiate,
        };
        self.originate(protocol::EVENT_INITIATED, &signed)?;
        Ok(signed.initiate.id)
    }

    pub fn submit_payment(
        &mut self,
        settlement_id: SettlementId,
        payment_reference: Option<String>,
    ) -> Result<(), SettlementError> {
        let action = PaymentSubmitted {
            settlement_id,
            buyer: self.gossip.node.local_peer_id(),
            payment_reference,
            timestamp: Timestamp::now(),
        };
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::PAYMENT_SUBMITTED,
            &action,
        )
        .map_err(|_| SettlementError::MalformedSettlement)?;
        let signed = SignedPaymentSubmitted {
            signature: self.gossip.sign(&bytes),
            action,
        };
        self.originate(protocol::EVENT_PAYMENT_SUBMITTED, &signed)
    }

    pub fn reverse_payment(&mut self, settlement_id: SettlementId) -> Result<(), SettlementError> {
        let action = PaymentReversed {
            settlement_id,
            buyer: self.gossip.node.local_peer_id(),
            timestamp: Timestamp::now(),
        };
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::PAYMENT_REVERSED,
            &action,
        )
        .map_err(|_| SettlementError::MalformedSettlement)?;
        let signed = SignedPaymentReversed {
            signature: self.gossip.sign(&bytes),
            action,
        };
        self.originate(protocol::EVENT_PAYMENT_REVERSED, &signed)
    }

    pub fn approve(&mut self, settlement_id: SettlementId) -> Result<(), SettlementError> {
        let action = SettlementApproved {
            settlement_id,
            seller: self.gossip.node.local_peer_id(),
            timestamp: Timestamp::now(),
        };
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::SETTLEMENT_APPROVED,
            &action,
        )
        .map_err(|_| SettlementError::MalformedSettlement)?;
        let signed = SignedSettlementApproved {
            signature: self.gossip.sign(&bytes),
            action,
        };
        self.originate(protocol::EVENT_APPROVED, &signed)
    }

    /// Records that this settlement's on-chain `release_escrow`
    /// transaction has been independently observed as confirmed
    /// (OFS-4300 §7-8) — local bookkeeping, not gossiped, since every
    /// node can verify chain confirmation for itself; see
    /// `SettlementRegistry::apply_escrow_released`.
    pub fn record_escrow_released(
        &mut self,
        settlement_id: &SettlementId,
        signature: impl Into<String>,
    ) -> Result<(), SettlementError> {
        self.registry
            .apply_escrow_released(settlement_id, signature)
    }

    /// `discrepancy` classifies the rejection for OFS-3000 §14; pass
    /// `PaymentDiscrepancy::Other` when the rejection is not about the
    /// payment's details, so it doesn't count against the payer.
    pub fn reject(
        &mut self,
        settlement_id: SettlementId,
        reason: String,
        discrepancy: PaymentDiscrepancy,
    ) -> Result<(), SettlementError> {
        let action = SettlementRejected {
            settlement_id,
            seller: self.gossip.node.local_peer_id(),
            reason,
            discrepancy,
            timestamp: Timestamp::now(),
        };
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::SETTLEMENT_REJECTED,
            &action,
        )
        .map_err(|_| SettlementError::MalformedSettlement)?;
        let signed = SignedSettlementRejected {
            signature: self.gossip.sign(&bytes),
            action,
        };
        self.originate(protocol::EVENT_REJECTED, &signed)
    }

    pub fn cancel(&mut self, settlement_id: SettlementId) -> Result<(), SettlementError> {
        let action = SettlementCancelled {
            settlement_id,
            canceller: self.gossip.node.local_peer_id(),
            timestamp: Timestamp::now(),
        };
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::SETTLEMENT_CANCELLED,
            &action,
        )
        .map_err(|_| SettlementError::MalformedSettlement)?;
        let signed = SignedSettlementCancelled {
            signature: self.gossip.sign(&bytes),
            action,
        };
        self.originate(protocol::EVENT_CANCELLED, &signed)
    }

    fn originate(
        &mut self,
        event_type: &str,
        payload: &impl serde::Serialize,
    ) -> Result<(), SettlementError> {
        let bytes = wire::to_bytes(payload).map_err(|_| SettlementError::MalformedSettlement)?;
        let event_type = EventType::new(event_type)
            .expect("settlement event names are all valid PascalCase identifiers");
        self.gossip
            .originate(
                event_type,
                protocol::OFS_SPEC,
                Priority::SessionReservationSettlement,
                8,
                bytes,
            )
            .map(|_| ())
            .map_err(|_| SettlementError::Unauthorized)
    }

    pub async fn drive_once(&mut self) {
        self.gossip.drive_once().await;
    }
}
