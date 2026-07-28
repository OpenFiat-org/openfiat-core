//! Drives one node's gossip: origination (§8-9), duplicate suppression
//! (§11), TTL-bounded forwarding (§12-13), and recovery on (re)connect
//! (§17, §22) — the peer-exchange announcement pattern from
//! `openfiat-discovery` extended to full event catch-up rather than just
//! peer lists.

use crate::authorization;
use crate::channel::Subscription;
use crate::error::GossipError;
use crate::event_id;
use crate::protocol::{MESSAGE_TYPE_PUSH, MESSAGE_TYPE_RECOVERY_REQUEST, MESSAGE_TYPE_RECOVERY_RESPONSE, OFS_SPEC, RecoveryRequest, RecoveryResponse};
use crate::store::EventStore;
use libp2p::request_response::{self, Message, ResponseChannel};
use libp2p::swarm::SwarmEvent;
use openfiat_crypto::{Keypair, verify};
use openfiat_network::behaviour::OpenFiatBehaviourEvent;
use openfiat_network::{Envelope, Node, PeerId as Libp2pPeerId};
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{EventEnvelope, EventId, EventType, NodeRole, PeerId, Priority, PublicKey, Timestamp};
use std::collections::{HashMap, HashSet};

/// What happened when an event was offered to [`GossipService::receive_event`].
#[derive(Debug)]
pub enum ReceiveOutcome {
    Stored,
    Duplicate,
    Rejected(GossipError),
}

pub struct GossipService<S> {
    pub node: Node,
    store: EventStore<S>,
    keypair: Keypair,
    self_peer_id: PeerId,
    self_roles: Vec<NodeRole>,
    subscription: Subscription,
    /// Known peers' public keys, needed to verify signatures on events
    /// they originate. In a real node this is populated from
    /// `openfiat-discovery`'s peer cache; kept as a plain map here rather
    /// than a second `KvStore` generic so this crate doesn't need to know
    /// discovery's storage backing.
    peer_keys: HashMap<PeerId, PublicKey>,
    connected: HashSet<Libp2pPeerId>,
    /// Invoked for every event this node stores — whether self-originated
    /// or received (pushed, or recovered) — so a crate built on top of
    /// gossip (registry, advertisements, ...) can react without gossip
    /// needing to know anything about what's built on it.
    event_handler: Option<EventHandler>,
}

/// A callback notified of every event a [`GossipService`] stores.
type EventHandler = Box<dyn FnMut(&EventEnvelope)>;

impl<S: KvStore> GossipService<S> {
    pub fn new(node: Node, store: EventStore<S>, keypair: Keypair, self_roles: Vec<NodeRole>, subscription: Subscription) -> Self {
        let self_peer_id = node.local_peer_id();
        Self {
            node,
            store,
            keypair,
            self_peer_id,
            self_roles,
            subscription,
            peer_keys: HashMap::new(),
            connected: HashSet::new(),
            event_handler: None,
        }
    }

    /// This node's public key, for crates built on top of gossip that need
    /// to embed it in their own signed payloads (e.g. a Service Registry
    /// registration).
    pub fn public_key(&self) -> PublicKey {
        self.keypair.public_key()
    }

    /// Sign a message with this node's identity keypair — for crates built
    /// on top of gossip that need their own signed payloads authenticated
    /// by the same node identity, without exposing the keypair itself.
    pub fn sign(&self, message: &[u8]) -> openfiat_types::Signature {
        self.keypair.sign(message)
    }

    /// Set the handler notified of every event this node stores (see the
    /// `event_handler` field doc). Replaces any previously set handler.
    pub fn set_event_handler(&mut self, handler: impl FnMut(&EventEnvelope) + 'static) {
        self.event_handler = Some(Box::new(handler));
    }

    /// Register a peer's public key so events it originates can be
    /// verified (see the `peer_keys` field doc).
    pub fn register_peer_key(&mut self, peer_id: PeerId, public_key: PublicKey) {
        self.peer_keys.insert(peer_id, public_key);
    }

    pub fn event_count(&self) -> usize {
        self.store.all().len()
    }

    pub fn has_event(&self, id: &EventId) -> bool {
        self.store.contains(id)
    }

    pub fn get_event(&self, id: &EventId) -> Option<EventEnvelope> {
        self.store.get(id)
    }

    pub fn connected_peer_count(&self) -> usize {
        self.connected.len()
    }

    /// Gracefully disconnect from every currently connected peer.
    pub fn disconnect_all(&mut self) {
        for peer in self.connected.clone() {
            let _ = self.node.graceful_disconnect(peer);
        }
    }

    /// Originate a new event (§8: Created → Local Validation → Signed →
    /// Stored → Broadcast). Only the origin's own broadcast carries the
    /// full `ttl` unchanged; every subsequent hop decrements it (§12).
    pub fn originate(&mut self, event_type: EventType, ofs_spec: u16, priority: Priority, ttl: u8, payload: Vec<u8>) -> Result<EventId, GossipError> {
        if !authorization::is_authorized(&self.self_roles, &event_type) {
            return Err(GossipError::UnauthorizedOrigination);
        }

        let timestamp = Timestamp::now();
        let signable = event_id::signable_bytes(&event_type, ofs_spec, &self.self_peer_id, timestamp, &payload);
        let signature = self.keypair.sign(&signable);
        let id = event_id::compute(&event_type, &payload, timestamp, &self.self_peer_id, &signature);

        let envelope = EventEnvelope {
            id,
            event_type,
            ofs_spec,
            version: 1,
            origin: self.self_peer_id.clone(),
            timestamp,
            ttl,
            priority,
            signature,
            payload,
        };

        self.store.put(&envelope);
        self.notify(&envelope);
        self.broadcast(&envelope, None);
        Ok(id)
    }

    fn notify(&mut self, event: &EventEnvelope) {
        if let Some(handler) = &mut self.event_handler {
            handler(event);
        }
    }

    /// Offer a received event for validation, dedup, storage, and
    /// TTL-bounded re-forwarding (§8-13).
    pub fn receive_event(&mut self, from: Option<Libp2pPeerId>, event: EventEnvelope) -> ReceiveOutcome {
        if self.store.contains(&event.id) {
            return ReceiveOutcome::Duplicate;
        }
        if let Err(err) = self.validate(&event) {
            return ReceiveOutcome::Rejected(err);
        }
        self.store.put(&event);
        self.notify(&event);
        self.forward(from, &event);
        ReceiveOutcome::Stored
    }

    /// §9 local validation, applied identically to received events:
    /// protocol version and signature. Full "event authorization" for a
    /// *remote* origin (was this peer actually allowed to emit this event
    /// type?) needs the Service Registry (Phase 5) to know what roles a
    /// remote `PeerId` holds — [`authorization::is_authorized`] is only
    /// applied at local origination for now.
    fn validate(&self, event: &EventEnvelope) -> Result<(), GossipError> {
        if event.version != 1 {
            return Err(GossipError::ProtocolVersionMismatch);
        }
        let public_key = self.peer_keys.get(&event.origin).ok_or(GossipError::InvalidSignature)?;
        let signable = event_id::signable_bytes(&event.event_type, event.ofs_spec, &event.origin, event.timestamp, &event.payload);
        verify(public_key, &signable, &event.signature).map_err(|_| GossipError::InvalidSignature)
    }

    /// Re-forward a received event, decrementing its TTL first (§12).
    /// Never sent back to whoever we received it from (§13).
    fn forward(&mut self, from: Option<Libp2pPeerId>, event: &EventEnvelope) {
        let Some(next_ttl) = event.ttl.checked_sub(1).filter(|&ttl| ttl > 0) else {
            return;
        };
        let mut forwarded = event.clone();
        forwarded.ttl = next_ttl;
        self.broadcast(&forwarded, from);
    }

    fn broadcast(&mut self, event: &EventEnvelope, exclude: Option<Libp2pPeerId>) {
        let payload = wire::to_bytes(event).expect("EventEnvelope always serializes");
        for peer in self.connected.clone() {
            if Some(peer) == exclude {
                continue;
            }
            self.node.send_envelope(peer, Envelope::new(OFS_SPEC, MESSAGE_TYPE_PUSH, 1, payload.clone()));
        }
    }

    pub async fn drive_once(&mut self) {
        let event = self.node.next_event().await;
        self.handle(event);
    }

    fn handle(&mut self, event: SwarmEvent<OpenFiatBehaviourEvent>) {
        match event {
            SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                self.connected.insert(peer_id);
                self.request_recovery(peer_id);
            }
            SwarmEvent::ConnectionClosed { peer_id, .. } => {
                self.connected.remove(&peer_id);
            }
            SwarmEvent::Behaviour(OpenFiatBehaviourEvent::Envelope(request_response::Event::Message { peer, message, .. })) => {
                match message {
                    Message::Request { request, channel, .. } => self.on_request(peer, request, channel),
                    Message::Response { response, .. } => self.on_response(response),
                }
            }
            _ => {}
        }
    }

    /// "Nodes recovering after downtime SHALL request missing events" (§22)
    /// — sent on every fresh connection, which doubles as §17's partition
    /// recovery ("connectivity restored → missing events exchanged").
    fn request_recovery(&mut self, peer: Libp2pPeerId) {
        let payload = wire::to_bytes(&RecoveryRequest { subscription: self.subscription.clone() }).expect("RecoveryRequest always serializes");
        self.node.send_envelope(peer, Envelope::new(OFS_SPEC, MESSAGE_TYPE_RECOVERY_REQUEST, 1, payload));
    }

    fn on_request(&mut self, peer: Libp2pPeerId, envelope: Envelope, channel: ResponseChannel<Envelope>) {
        match envelope.header.message_type.as_str() {
            MESSAGE_TYPE_PUSH => {
                if let Ok(event) = wire::from_bytes::<EventEnvelope>(&envelope.payload) {
                    self.receive_event(Some(peer), event);
                }
                // Pushes are fire-and-forget; dropping `channel` here is a
                // valid, harmless outcome (OFNP request-response semantics).
            }
            MESSAGE_TYPE_RECOVERY_REQUEST => {
                if let Ok(request) = wire::from_bytes::<RecoveryRequest>(&envelope.payload) {
                    let events = self.store.all_for_subscription(&request.subscription);
                    let payload = wire::to_bytes(&RecoveryResponse { events }).expect("RecoveryResponse always serializes");
                    let response = Envelope::new(OFS_SPEC, MESSAGE_TYPE_RECOVERY_RESPONSE, 1, payload);
                    let _ = self.node.swarm.behaviour_mut().envelope.send_response(channel, response);
                }
            }
            _ => {}
        }
    }

    fn on_response(&mut self, envelope: Envelope) {
        if envelope.header.message_type == MESSAGE_TYPE_RECOVERY_RESPONSE
            && let Ok(response) = wire::from_bytes::<RecoveryResponse>(&envelope.payload)
        {
            for event in response.events {
                self.receive_event(None, event);
            }
        }
    }
}
