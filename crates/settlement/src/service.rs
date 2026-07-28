//! Drives one node's settlement index: applies incoming gossip events
//! automatically and provides the operations that originate new ones.

use crate::error::SettlementError;
use crate::events::{
    PaymentReversed, PaymentSubmitted, SettlementApproved, SettlementCancelled, SettlementInitiate, SettlementRejected, SignedPaymentReversed,
    SignedPaymentSubmitted, SignedSettlementApproved, SignedSettlementCancelled, SignedSettlementInitiate, SignedSettlementRejected,
};
use crate::protocol;
use crate::record::{Settlement, SettlementId};
use crate::store::SettlementRegistry;
use openfiat_gossip::GossipService;
use openfiat_reservations::ReservationId;
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{Amount, EventType, PeerId, Priority, PublicKey, Timestamp};
use std::rc::Rc;

pub struct SettlementService<S> {
    pub gossip: GossipService<S>,
    registry: Rc<SettlementRegistry<S>>,
}

impl<S: KvStore + 'static> SettlementService<S> {
    pub fn new(mut gossip: GossipService<S>, store: S) -> Self {
        let registry = Rc::new(SettlementRegistry::new(store));
        let handler_registry = Rc::clone(&registry);
        gossip.set_event_handler(move |event| handler_registry.apply_event(event));
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
        let bytes = wire::to_bytes(&initiate).expect("SettlementInitiate always serializes");
        let signed = SignedSettlementInitiate { signature: self.gossip.sign(&bytes), initiate };
        self.originate(protocol::EVENT_INITIATED, &signed)?;
        Ok(signed.initiate.id)
    }

    pub fn submit_payment(&mut self, settlement_id: SettlementId, payment_reference: Option<String>) -> Result<(), SettlementError> {
        let action = PaymentSubmitted { settlement_id, buyer: self.gossip.node.local_peer_id(), payment_reference, timestamp: Timestamp::now() };
        let bytes = wire::to_bytes(&action).map_err(|_| SettlementError::MalformedSettlement)?;
        let signed = SignedPaymentSubmitted { signature: self.gossip.sign(&bytes), action };
        self.originate(protocol::EVENT_PAYMENT_SUBMITTED, &signed)
    }

    pub fn reverse_payment(&mut self, settlement_id: SettlementId) -> Result<(), SettlementError> {
        let action = PaymentReversed { settlement_id, buyer: self.gossip.node.local_peer_id(), timestamp: Timestamp::now() };
        let bytes = wire::to_bytes(&action).map_err(|_| SettlementError::MalformedSettlement)?;
        let signed = SignedPaymentReversed { signature: self.gossip.sign(&bytes), action };
        self.originate(protocol::EVENT_PAYMENT_REVERSED, &signed)
    }

    pub fn approve(&mut self, settlement_id: SettlementId) -> Result<(), SettlementError> {
        let action = SettlementApproved { settlement_id, seller: self.gossip.node.local_peer_id(), timestamp: Timestamp::now() };
        let bytes = wire::to_bytes(&action).map_err(|_| SettlementError::MalformedSettlement)?;
        let signed = SignedSettlementApproved { signature: self.gossip.sign(&bytes), action };
        self.originate(protocol::EVENT_APPROVED, &signed)
    }

    pub fn reject(&mut self, settlement_id: SettlementId, reason: String) -> Result<(), SettlementError> {
        let action = SettlementRejected { settlement_id, seller: self.gossip.node.local_peer_id(), reason, timestamp: Timestamp::now() };
        let bytes = wire::to_bytes(&action).map_err(|_| SettlementError::MalformedSettlement)?;
        let signed = SignedSettlementRejected { signature: self.gossip.sign(&bytes), action };
        self.originate(protocol::EVENT_REJECTED, &signed)
    }

    pub fn cancel(&mut self, settlement_id: SettlementId) -> Result<(), SettlementError> {
        let action = SettlementCancelled { settlement_id, canceller: self.gossip.node.local_peer_id(), timestamp: Timestamp::now() };
        let bytes = wire::to_bytes(&action).map_err(|_| SettlementError::MalformedSettlement)?;
        let signed = SignedSettlementCancelled { signature: self.gossip.sign(&bytes), action };
        self.originate(protocol::EVENT_CANCELLED, &signed)
    }

    fn originate(&mut self, event_type: &str, payload: &impl serde::Serialize) -> Result<(), SettlementError> {
        let bytes = wire::to_bytes(payload).map_err(|_| SettlementError::MalformedSettlement)?;
        let event_type = EventType::new(event_type).expect("settlement event names are all valid PascalCase identifiers");
        self.gossip
            .originate(event_type, protocol::OFS_SPEC, Priority::SessionReservationSettlement, 8, bytes)
            .map(|_| ())
            .map_err(|_| SettlementError::Unauthorized)
    }

    pub async fn drive_once(&mut self) {
        self.gossip.drive_once().await;
    }
}
