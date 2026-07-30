//! Drives one node's gossip: origination (§8-9), duplicate suppression
//! (§11), TTL-bounded forwarding (§12-13), and recovery on (re)connect
//! (§17, §22) — the peer-exchange announcement pattern from
//! `openfiat-discovery` extended to full event catch-up rather than just
//! peer lists.

use crate::authorization;
use crate::channel::Subscription;
use crate::error::GossipError;
use crate::event_id;
use crate::protocol::{
    MESSAGE_TYPE_PUSH, MESSAGE_TYPE_RECOVERY_REQUEST, MESSAGE_TYPE_RECOVERY_RESPONSE, OFS_SPEC,
    RecoveryRequest, RecoveryResponse,
};
use crate::store::EventStore;
use libp2p::request_response::{self, Message, ResponseChannel};
use libp2p::swarm::SwarmEvent;
use openfiat_crypto::{Keypair, verify};
use openfiat_network::behaviour::OpenFiatBehaviourEvent;
use openfiat_network::identity::{from_libp2p_peer_id, is_dialable, public_key_from_peer_id};
use openfiat_network::{Envelope, Multiaddr, Node, PeerId as Libp2pPeerId};
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{
    EventEnvelope, EventId, EventType, NodeRole, PeerId, Priority, PublicKey, Timestamp,
};
use std::collections::{BTreeSet, HashMap, HashSet};

/// What happened when an event was offered to [`GossipService::receive_event`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// they originate. Auto-populated on every new connection (see
    /// `handle`'s `ConnectionEstablished` arm, `public_key_from_peer_id`)
    /// since Ed25519 peer ids embed their own key; `openfiat-discovery`'s
    /// peer cache or an explicit `register_peer_key` call (as tests do)
    /// can also seed it ahead of a connection. Kept as a plain map here
    /// rather than a second `KvStore` generic so this crate doesn't need
    /// to know discovery's storage backing.
    peer_keys: HashMap<PeerId, PublicKey>,
    connected: HashSet<Libp2pPeerId>,
    /// Addresses at which this node is actually reachable, learned rather
    /// than configured.
    ///
    /// Two independent sources, and the difference matters. `NewListenAddr`
    /// is what libp2p bound after expanding `--gossip-bind-address`: bind
    /// `0.0.0.0` and it reports one concrete address per interface, which
    /// is the answer for a host whose interface address is its real one.
    /// identify's `observed_addr` is what a *peer* saw the connection
    /// arrive from, which is the only way to learn a public address behind
    /// NAT — no amount of local inspection can produce it.
    ///
    /// Bind wildcards never enter this set (see [`is_dialable`]): an
    /// address that cannot be dialled is worse than none, because it looks
    /// like an answer.
    reachable: BTreeSet<Multiaddr>,
    /// When this node started, used to tell its own history from an
    /// impostor's traffic — see [`GossipService::accept`].
    started_at: Timestamp,
    /// How many events signed by this node's own key, but not emitted by
    /// it, have arrived. Non-zero means the identity is running in more
    /// than one place.
    identity_conflicts: u64,
    /// Reachable addresses not yet handed to a caller, so each is reported
    /// once rather than on every tick of whatever is polling.
    newly_reachable: Vec<Multiaddr>,
    /// Invoked for every event this node stores — whether self-originated
    /// or received (pushed, or recovered) — so a crate built on top of
    /// gossip (registry, advertisements, ...) can react without gossip
    /// needing to know anything about what's built on it. A `Vec` rather
    /// than a single slot: a real node multiplexes every domain's events
    /// through one shared `GossipService` (that's the point of `ofs_spec`
    /// discrimination), so more than one domain crate needs to register a
    /// handler on the same instance without evicting the others'.
    event_handlers: Vec<EventHandler>,
    /// Consulted before re-forwarding a *received* event (never for a
    /// self-originated one — the origin's own first broadcast always
    /// goes out). All registered filters must agree to forward; any one
    /// returning `false` suppresses it. This exists for domain crates
    /// whose events are independently, repeatedly observed by many
    /// unrelated origins for the *same underlying fact* (OFS-4300 §6's
    /// blockhash announcements) — ordinary dedup is keyed by `EventId`,
    /// which differs per origin/signature/timestamp even for identical
    /// content, so it does not by itself bound that kind of redundancy.
    forward_filters: Vec<ForwardFilter>,
}

/// A callback notified of every event a [`GossipService`] stores.
type EventHandler = Box<dyn FnMut(&EventEnvelope)>;

/// A callback that may veto re-forwarding a received event (see
/// `forward_filters`).
type ForwardFilter = Box<dyn FnMut(&EventEnvelope) -> bool>;

impl<S: KvStore> GossipService<S> {
    pub fn new(
        node: Node,
        store: EventStore<S>,
        keypair: Keypair,
        self_roles: Vec<NodeRole>,
        subscription: Subscription,
    ) -> Self {
        let self_peer_id = node.local_peer_id();
        // This node's own key goes in the map beside its peers'.
        //
        // Not vanity: `validate` looks the origin's key up here, so
        // without it an event claiming our origin fails as
        // `InvalidSignature` and we can never tell a clumsy spoof from
        // proof that our wallet is running somewhere else. That
        // distinction is the entire value of `is_impostor`, and it is
        // only available once the signature has actually been checked —
        // which is also why the impostor test runs *after* validation
        // rather than before it. Checking first would let anyone trigger
        // a false alarm on our node by putting our peer id in an
        // envelope they never signed.
        let mut peer_keys = HashMap::new();
        peer_keys.insert(self_peer_id.clone(), keypair.public_key());
        Self {
            node,
            store,
            keypair,
            self_peer_id,
            self_roles,
            subscription,
            peer_keys,
            connected: HashSet::new(),
            reachable: BTreeSet::new(),
            started_at: Timestamp::now(),
            identity_conflicts: 0,
            newly_reachable: Vec::new(),
            event_handlers: Vec::new(),
            forward_filters: Vec::new(),
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

    /// Register a handler notified of every event this node stores (see
    /// the `event_handlers` field doc). Appends — every previously
    /// registered handler keeps running.
    pub fn add_event_handler(&mut self, handler: impl FnMut(&EventEnvelope) + 'static) {
        self.event_handlers.push(Box::new(handler));
    }

    /// Register a filter that may veto re-forwarding a *received* event
    /// (see the `forward_filters` field doc). Appends — every previously
    /// registered filter keeps running, and all of them must agree to
    /// forward.
    pub fn add_forward_filter(&mut self, filter: impl FnMut(&EventEnvelope) -> bool + 'static) {
        self.forward_filters.push(Box::new(filter));
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
    pub fn originate(
        &mut self,
        event_type: EventType,
        ofs_spec: u16,
        priority: Priority,
        ttl: u8,
        payload: Vec<u8>,
    ) -> Result<EventId, GossipError> {
        if !authorization::is_authorized(&self.self_roles, &event_type) {
            return Err(GossipError::UnauthorizedOrigination);
        }

        let timestamp = Timestamp::now();
        let signable = event_id::signable_bytes(
            &event_type,
            ofs_spec,
            &self.self_peer_id,
            timestamp,
            &payload,
        );
        let signature = self.keypair.sign(&signable);
        let id = event_id::compute(
            &event_type,
            &payload,
            timestamp,
            &self.self_peer_id,
            &signature,
        );

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
        for handler in &mut self.event_handlers {
            handler(event);
        }
    }

    /// Offer a received event for validation, dedup, storage, and
    /// TTL-bounded re-forwarding (§8-13).
    pub fn receive_event(
        &mut self,
        from: Option<Libp2pPeerId>,
        event: EventEnvelope,
    ) -> ReceiveOutcome {
        if self.store.contains(&event.id) {
            return ReceiveOutcome::Duplicate;
        }
        if let Err(err) = self.validate(&event) {
            return ReceiveOutcome::Rejected(err);
        }
        if self.is_impostor(&event) {
            self.identity_conflicts += 1;
            return ReceiveOutcome::Rejected(GossipError::IdentityInUseElsewhere);
        }
        self.store.put(&event);
        self.notify(&event);
        if self.should_forward(&event) {
            self.forward(from, &event);
        }
        ReceiveOutcome::Stored
    }

    /// Whether `event` was signed by this node's key but not emitted by
    /// this node — meaning a second node is running the same identity.
    ///
    /// One wallet is one node. A `PeerId` is derived from the wallet's
    /// key, so two nodes sharing a `wallet.json` do not appear as two
    /// peers: they appear as one peer in two places, both signing under
    /// the same name. Nothing in an envelope distinguishes them, which is
    /// exactly why this has to be detected from the one vantage point
    /// that can: our own.
    ///
    /// The test is precise. Anything this node originated went into the
    /// store at origination, so an echo of it is already `Duplicate`
    /// before reaching here. An event still claiming our origin is
    /// therefore one we did not emit, and if it is stamped after we
    /// booted, we would have known about it. That last clause is what
    /// keeps an honest restart from accusing itself: a node that lost its
    /// data directory and restarted on the same wallet will meet its own
    /// older events again, and those are stamped before this boot.
    ///
    /// Two nodes running one wallet is not a configuration to support. It
    /// makes gossip attributable to a peer that is two machines, splits
    /// one stake across both in any accounting that keys on identity, and
    /// means a compromise of either is indistinguishable from the other.
    /// The event is refused and the operator is told.
    fn is_impostor(&self, event: &EventEnvelope) -> bool {
        event.origin == self.self_peer_id && event.timestamp > self.started_at
    }

    /// How many events signed by this identity, but not emitted here,
    /// have been seen. Any non-zero value means the wallet is in use
    /// somewhere else.
    pub fn identity_conflicts(&self) -> u64 {
        self.identity_conflicts
    }

    /// Whether every registered forward filter agrees to re-forward
    /// `event`. Vacuously `true` when no filter is registered, so this
    /// changes nothing for domains that never call `add_forward_filter`.
    fn should_forward(&mut self, event: &EventEnvelope) -> bool {
        self.forward_filters.iter_mut().all(|filter| filter(event))
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
        let public_key = self
            .peer_keys
            .get(&event.origin)
            .ok_or(GossipError::InvalidSignature)?;
        let signable = event_id::signable_bytes(
            &event.event_type,
            event.ofs_spec,
            &event.origin,
            event.timestamp,
            &event.payload,
        );
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
            self.node.send_envelope(
                peer,
                Envelope::new(OFS_SPEC, MESSAGE_TYPE_PUSH, 1, payload.clone()),
            );
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
                // Two independently-started nodes have no advance
                // knowledge of each other's signing key — recover it
                // from the connection's own peer id (see
                // `public_key_from_peer_id`'s doc) so `validate` can
                // verify events this peer originates without a prior,
                // separate key-exchange step.
                if let Some(public_key) = public_key_from_peer_id(peer_id) {
                    self.register_peer_key(from_libp2p_peer_id(peer_id), public_key);
                }
                self.request_recovery(peer_id);
            }
            SwarmEvent::ConnectionClosed { peer_id, .. } => {
                self.connected.remove(&peer_id);
            }
            // What libp2p actually bound, one event per interface once a
            // wildcard is expanded.
            SwarmEvent::NewListenAddr { address, .. } => {
                self.record_reachable(address);
            }
            // What a peer saw. The only source that can see through NAT,
            // and unverified by design: a peer could report anything. It
            // costs nothing to be wrong here — the address is used to tell
            // an operator where they appear to be reachable, never to
            // decide anything — but that is why it is not treated as
            // authoritative anywhere else.
            SwarmEvent::Behaviour(OpenFiatBehaviourEvent::Identify(
                libp2p::identify::Event::Received { info, .. },
            )) => {
                self.record_reachable(info.observed_addr);
            }
            SwarmEvent::Behaviour(OpenFiatBehaviourEvent::Envelope(
                request_response::Event::Message { peer, message, .. },
            )) => match message {
                Message::Request {
                    request, channel, ..
                } => self.on_request(peer, request, channel),
                Message::Response { response, .. } => self.on_response(response),
            },
            _ => {}
        }
    }

    fn record_reachable(&mut self, address: Multiaddr) {
        if !is_dialable(&address) {
            return;
        }
        if self.reachable.insert(address.clone()) {
            self.newly_reachable.push(address);
        }
    }

    /// Every address this node is known to be reachable at.
    pub fn reachable_addresses(&self) -> Vec<Multiaddr> {
        self.reachable.iter().cloned().collect()
    }

    /// Addresses learned since the last call, draining them.
    ///
    /// Draining rather than returning the whole set so a caller that logs
    /// them reports each once. A node re-announcing the same address every
    /// tick would bury everything else it says.
    pub fn take_newly_reachable(&mut self) -> Vec<Multiaddr> {
        std::mem::take(&mut self.newly_reachable)
    }

    /// "Nodes recovering after downtime SHALL request missing events" (§22)
    /// — sent on every fresh connection, which doubles as §17's partition
    /// recovery ("connectivity restored → missing events exchanged").
    fn request_recovery(&mut self, peer: Libp2pPeerId) {
        let payload = wire::to_bytes(&RecoveryRequest {
            subscription: self.subscription.clone(),
        })
        .expect("RecoveryRequest always serializes");
        self.node.send_envelope(
            peer,
            Envelope::new(OFS_SPEC, MESSAGE_TYPE_RECOVERY_REQUEST, 1, payload),
        );
    }

    fn on_request(
        &mut self,
        peer: Libp2pPeerId,
        envelope: Envelope,
        channel: ResponseChannel<Envelope>,
    ) {
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
                    let payload = wire::to_bytes(&RecoveryResponse { events })
                        .expect("RecoveryResponse always serializes");
                    let response =
                        Envelope::new(OFS_SPEC, MESSAGE_TYPE_RECOVERY_RESPONSE, 1, payload);
                    let _ = self
                        .node
                        .swarm
                        .behaviour_mut()
                        .envelope
                        .send_response(channel, response);
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

#[cfg(test)]
/// One wallet is one node, enforced from the only vantage point that
/// can tell: the node whose identity is being used.
mod identity_conflicts {
    use super::*;
    use openfiat_storage::mem::MemoryStore;

    /// Builds an event genuinely signed by `keypair` — an impostor
    /// holding a copied `wallet.json` produces exactly this, and it
    /// passes every signature check, because the signature is real.
    fn signed_as(keypair: &Keypair, at: Timestamp) -> EventEnvelope {
        let peer =
            openfiat_network::identity::peer_id_from_public_key(&keypair.public_key()).unwrap();
        let event_type = EventType::new("AdvertisementCreated").unwrap();
        let payload = b"from the other machine".to_vec();
        let signable = event_id::signable_bytes(&event_type, 2100, &peer, at, &payload);
        let signature = keypair.sign(&signable);
        let id = event_id::compute(&event_type, &payload, at, &peer, &signature);
        EventEnvelope {
            id,
            event_type,
            ofs_spec: 2100,
            version: 1,
            origin: peer,
            timestamp: at,
            ttl: 8,
            priority: Priority::Advertisement,
            signature,
            payload,
        }
    }

    fn service_for(keypair: &Keypair) -> GossipService<MemoryStore> {
        let node = openfiat_network::Node::new(keypair).unwrap();
        GossipService::new(
            node,
            EventStore::new(MemoryStore::new()),
            Keypair::from_seed(keypair.seed()),
            vec![NodeRole::MerchantGateway],
            Subscription::All,
        )
    }

    #[test]
    fn an_event_signed_by_our_own_key_that_we_did_not_emit_is_refused() {
        let keypair = Keypair::from_seed([21u8; 32]);
        let mut service = service_for(&keypair);

        // Stamped a second after this node booted: the other machine is
        // running right alongside us. Explicitly later rather than
        // `now()`, because both can land in the same millisecond and the
        // rule is "after we booted", not "at or after".
        let after_boot = Timestamp::from_millis(service.started_at.as_millis() + 1_000);
        let forged = signed_as(&keypair, after_boot);
        let outcome = service.receive_event(None, forged.clone());

        assert_eq!(
            outcome,
            ReceiveOutcome::Rejected(GossipError::IdentityInUseElsewhere)
        );
        assert_eq!(service.identity_conflicts(), 1);
        assert!(
            !service.store.contains(&forged.id),
            "acting on an instruction issued under our name by someone \
             else is the one thing a node must never do"
        );
    }

    #[test]
    fn our_own_older_events_are_not_mistaken_for_an_impostor() {
        // The restart case: a node that lost its data directory and
        // came back on the same wallet meets its own history again.
        // Accusing itself here would make recovery impossible.
        let keypair = Keypair::from_seed([22u8; 32]);
        let mut service = service_for(&keypair);

        let before_boot = Timestamp::from_millis(service.started_at.as_millis() - 60_000);
        let own_history = signed_as(&keypair, before_boot);

        assert_eq!(
            service.receive_event(None, own_history),
            ReceiveOutcome::Stored
        );
        assert_eq!(service.identity_conflicts(), 0);
    }

    #[test]
    fn another_peers_event_is_untouched_by_the_check() {
        let ours = Keypair::from_seed([23u8; 32]);
        let theirs = Keypair::from_seed([24u8; 32]);
        let mut service = service_for(&ours);

        service.register_peer_key(
            openfiat_network::identity::peer_id_from_public_key(&theirs.public_key()).unwrap(),
            theirs.public_key(),
        );
        let after_boot = Timestamp::from_millis(service.started_at.as_millis() + 1_000);
        let legitimate = signed_as(&theirs, after_boot);
        assert_eq!(
            service.receive_event(None, legitimate),
            ReceiveOutcome::Stored
        );
        assert_eq!(service.identity_conflicts(), 0);
    }

    #[test]
    fn an_echo_of_our_own_broadcast_is_a_duplicate_not_an_accusation() {
        // Our own events go into the store at origination, so a peer
        // reflecting one back is caught as a duplicate before the
        // impostor check ever runs. Without that ordering, every node
        // would accuse itself the moment its own event came back.
        let keypair = Keypair::from_seed([25u8; 32]);
        let mut service = service_for(&keypair);

        let id = service
            .originate(
                EventType::new("AdvertisementCreated").unwrap(),
                2100,
                Priority::Advertisement,
                8,
                b"ours".to_vec(),
            )
            .unwrap();
        let echoed = service
            .store
            .get(&id)
            .expect("we stored what we originated");

        assert_eq!(
            service.receive_event(None, echoed),
            ReceiveOutcome::Duplicate
        );
        assert_eq!(service.identity_conflicts(), 0);
    }
}
