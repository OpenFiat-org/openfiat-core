//! Drives one node's reservation index: applies incoming gossip events
//! automatically and provides the request/cancel operations that
//! originate new ones.

use crate::error::ReservationError;
use crate::events::{
    ReservationCancel, ReservationRequest, SignedReservationCancel, SignedReservationRequest,
};
use crate::protocol;
use crate::record::{Reservation, ReservationId};
use crate::store::ReservationRegistry;
use openfiat_advertisements::AdvertisementId;
use openfiat_advertisements::AdvertisementRegistry;
use openfiat_gossip::GossipService;
use openfiat_serialization::json;
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{Amount, EventType, Priority, Timestamp};
use std::rc::Rc;

pub struct ReservationService<S> {
    pub gossip: GossipService<S>,
    registry: Rc<ReservationRegistry<S>>,
}

impl<S: KvStore + 'static> ReservationService<S> {
    /// `advertisements` is the shared handle from `AdvertisementService::registry`
    /// on the same node — this service validates against and adjusts that
    /// same replica rather than maintaining a separate copy.
    pub fn new(
        mut gossip: GossipService<S>,
        store: S,
        advertisements: Rc<AdvertisementRegistry<S>>,
    ) -> Self {
        let registry = Rc::new(ReservationRegistry::new(store, advertisements));
        let handler_registry = Rc::clone(&registry);
        gossip.add_event_handler(move |event| handler_registry.apply_event(event));
        Self { gossip, registry }
    }

    pub fn get(&self, id: &ReservationId) -> Option<Reservation> {
        self.registry.get(id)
    }

    pub fn all(&self) -> Vec<Reservation> {
        self.registry.all()
    }

    /// A shared handle to this node's reservation index, for crates
    /// downstream in the dependency chain (`openfiat-trade`) that
    /// correlate reservations with their settlements rather than
    /// re-deriving reservation state themselves.
    pub fn registry(&self) -> Rc<ReservationRegistry<S>> {
        Rc::clone(&self.registry)
    }

    pub fn expire_stale(&self) -> usize {
        self.registry.expire_stale(protocol::VALIDATION_WINDOW)
    }

    pub fn request(
        &mut self,
        id: impl Into<String>,
        advertisement_id: AdvertisementId,
        amount: Amount,
    ) -> Result<ReservationId, ReservationError> {
        let request = ReservationRequest {
            id: ReservationId::new(id),
            advertisement_id,
            requester: self.gossip.node.local_peer_id(),
            requester_public_key: self.gossip.public_key(),
            amount,
            timestamp: Timestamp::now(),
        };
        let bytes = json::to_bytes(&request).expect("ReservationRequest always serializes");
        let signed = SignedReservationRequest {
            signature: self.gossip.sign(&bytes),
            request,
        };
        self.originate(protocol::EVENT_REQUESTED, &signed)?;
        Ok(signed.request.id)
    }

    pub fn cancel(&mut self, id: ReservationId) -> Result<(), ReservationError> {
        let cancel = ReservationCancel {
            id,
            requester: self.gossip.node.local_peer_id(),
            timestamp: Timestamp::now(),
        };
        let bytes = json::to_bytes(&cancel).map_err(|_| ReservationError::MalformedReservation)?;
        let signed = SignedReservationCancel {
            signature: self.gossip.sign(&bytes),
            cancel,
        };
        self.originate(protocol::EVENT_CANCELLED, &signed)
    }

    fn originate(
        &mut self,
        event_type: &str,
        payload: &impl serde::Serialize,
    ) -> Result<(), ReservationError> {
        let bytes = wire::to_bytes(payload).map_err(|_| ReservationError::MalformedReservation)?;
        let event_type = EventType::new(event_type)
            .expect("reservation event names are all valid PascalCase identifiers");
        self.gossip
            .originate(
                event_type,
                protocol::OFS_SPEC,
                Priority::SessionReservationSettlement,
                8,
                bytes,
            )
            .map(|_| ())
            .map_err(|_| ReservationError::UnauthorizedUpdate)
    }

    pub async fn drive_once(&mut self) {
        self.gossip.drive_once().await;
    }
}
