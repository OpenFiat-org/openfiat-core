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
    MESSAGE_TYPE_PUSH, MESSAGE_TYPE_PUSH_ACK, MESSAGE_TYPE_RECOVERY_REQUEST,
    MESSAGE_TYPE_RECOVERY_RESPONSE, OFS_SPEC, RecoveryRequest, RecoveryResponse,
};
use crate::store::EventStore;
use libp2p::request_response::{self, Message, ResponseChannel};
use libp2p::swarm::SwarmEvent;
use openfiat_crypto::{Keypair, verify};
use openfiat_network::behaviour::OpenFiatBehaviourEvent;
use openfiat_network::identity::{is_dialable, public_key_from_peer_id};
use openfiat_network::{Envelope, Multiaddr, Node, PeerId as Libp2pPeerId};
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{
    EventEnvelope, EventId, EventType, NodeRole, PeerId, Priority, PublicKey, Timestamp,
};
use std::collections::{BTreeSet, HashMap, HashSet};

/// How many distinct peers must independently report seeing this node at
/// an address before it is one this node will act on.
///
/// Two, not one: one is a single peer's unverified word, and this node
/// publishes what it concludes from it (see
/// [`GossipService::corroborated_addresses`]). Not more than two, because
/// every increment is a node on a small or young cluster that never learns
/// its own public address at all, and the address is only ever a hint —
/// what it points at is verified on arrival regardless.
pub const MIN_OBSERVERS: usize = 2;

/// The largest hop budget this node will honour on a *received* event.
///
/// `ttl` is the one envelope field the protocol expects to change in
/// flight, so it cannot be signed, so any relay can write anything into
/// it. `docs/architecture.md` puts the default budget at 8 (OGP §12's own
/// illustrative figure); this is double that, which leaves room for a
/// network wider than anything measured while still bounding what one
/// inflated field can cost.
///
/// Clamped rather than rejected, and that direction is the whole point. A
/// node that refused an over-budget TTL would hand every relay a
/// censorship button: raise the field on someone else's genuinely signed
/// event and watch the rest of the network throw it away. Clamping costs
/// the liar nothing to attempt and gains them nothing either — the event
/// travels exactly as far as an honest one.
pub const MAX_TTL: u8 = 16;

/// How far ahead of this node's clock an event may be stamped.
///
/// Generous, because the cost of getting this wrong is refusing honest
/// traffic from a node whose clock is merely bad, and no node here runs
/// NTP by mandate. Bounded, because the event log is pruned by timestamp
/// and nothing else: an event stamped in the year 3000 is older than no
/// cutoff that will ever be computed, so it is a permanent row that one
/// unauthenticated push put there.
pub const MAX_CLOCK_SKEW_MILLIS: u64 = 5 * 60 * 1_000;

/// How many distinct addresses this node will remember having been told
/// it is reachable at.
///
/// `observed_addr` arrives from identify and is whatever the peer wrote —
/// a peer can report a fresh, well-formed, undialable-by-anyone address on
/// every reconnection forever. This is a diagnostic set shown to an
/// operator; the number of genuine answers is the number of interfaces
/// this host has, so a cap three orders of magnitude above that costs
/// nothing real and turns "grows until the node dies" into "the operator
/// sees the first 256 addresses anyone claimed".
pub const MAX_REACHABLE_ADDRESSES: usize = 256;

/// How many reporters are remembered per claimed address.
///
/// [`MIN_OBSERVERS`] is the only question this set answers, so anything
/// past a handful is recorded and never read. Without a cap it is a
/// per-address list of every peer id an attacker cares to mint.
const MAX_OBSERVERS_PER_ADDRESS: usize = 8;

/// The byte budget for one recovery response.
///
/// [`openfiat_network::MAX_ENVELOPE_BYTES`] is a hard 1 MiB and the codec
/// refuses to *write* anything larger, so before this existed a node whose
/// log had grown past a megabyte — which is every node that has been up a
/// day — answered every recovery request by building the whole log,
/// failing to encode it, and sending nothing at all. Recovery did not
/// degrade at scale, it stopped. This serves as much as will fit, oldest
/// first, and says so.
const RECOVERY_RESPONSE_BUDGET_BYTES: usize = 768 * 1024;

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
    /// Public keys supplied out of band, consulted only for an origin
    /// whose `PeerId` does not carry its own key.
    ///
    /// It used to be the *only* source, filled from `ConnectionEstablished`
    /// — which quietly meant a node could verify its direct peers and
    /// nobody else. An event relayed two hops names an origin this node
    /// has never connected to, so its key was absent and `validate`
    /// returned `InvalidSignature`: epidemic propagation past one hop did
    /// not work off the test harness, and every test in this workspace
    /// hid it by calling `register_peer_key` for the whole cluster by
    /// hand. [`Self::origin_key`] now derives the key from the origin
    /// itself, which is both the fix and strictly safer — a derived
    /// binding cannot be told a different key for an identity, and a map
    /// fed by remote input cannot grow without bound if nothing remote
    /// feeds it.
    peer_keys: HashMap<PeerId, PublicKey>,
    connected: HashSet<Libp2pPeerId>,
    /// Peers already given their one recovery response on the current
    /// connection.
    ///
    /// A recovery request is ~30 bytes and its answer is as much of the
    /// event log as fits in an envelope. Answering every request as it
    /// arrives makes this node a ~25,000x amplifier that any connected
    /// peer can point at itself for free, in a loop. The honest protocol
    /// asks exactly once, on connect ([`Self::request_recovery`]), so
    /// once per connection is the full honest need — and re-arming costs
    /// an attacker a real reconnection, handshake included, rather than
    /// nothing.
    ///
    /// Keyed on the connection, cleared in `ConnectionClosed`, so it is
    /// bounded by the peers actually connected rather than by everyone
    /// who has ever asked.
    recovery_served: HashSet<Libp2pPeerId>,
    /// Everything this node has been told it is reachable at, learned
    /// rather than configured — safe to show an operator, and not what
    /// anything decides on (that is `corroborated_addresses`).
    ///
    /// Two independent sources, and the difference matters. `NewListenAddr`
    /// is what libp2p bound after expanding `--gossip-bind-address`: bind
    /// `0.0.0.0` and it reports one concrete address per interface, which
    /// is the answer for a host whose interface address is its real one.
    /// identify's `observed_addr` is what a *peer* saw the connection
    /// arrive from, which is the only way to learn a public address behind
    /// NAT — no amount of local inspection can produce it, and no amount
    /// of local inspection can check it either.
    ///
    /// Bind wildcards never enter this set (see [`is_dialable`]): an
    /// address that cannot be dialled is worse than none, because it looks
    /// like an answer.
    reachable: BTreeSet<Multiaddr>,
    /// The subset of `reachable` that libp2p itself bound — a local fact,
    /// not a claim by anyone.
    bound: BTreeSet<Multiaddr>,
    /// Which peers reported observing this node at which address.
    ///
    /// Kept per reporter rather than as a count, because a count is
    /// something one peer can raise on its own by reconnecting.
    observed_by: HashMap<Multiaddr, HashSet<Libp2pPeerId>>,
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
        // Empty, and it stays empty on an ordinary node: `origin_key`
        // derives every key it needs from the origin's own `PeerId`,
        // including this node's.
        //
        // That an event claiming *our* origin can still be verified is
        // load-bearing, not incidental. `is_impostor` distinguishes a
        // clumsy spoof from proof that our wallet is running somewhere
        // else, and that distinction only exists once the signature has
        // actually been checked — which is why the impostor test runs
        // after validation rather than before it. Checking first would
        // let anyone raise a false alarm on our node by putting our peer
        // id in an envelope they never signed.
        let peer_keys = HashMap::new();
        Self {
            node,
            store,
            keypair,
            self_peer_id,
            self_roles,
            subscription,
            peer_keys,
            connected: HashSet::new(),
            recovery_served: HashSet::new(),
            reachable: BTreeSet::new(),
            bound: BTreeSet::new(),
            observed_by: HashMap::new(),
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
        mut event: EventEnvelope,
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
        // Before it is stored, so this node never relays a budget it
        // would not itself have granted, and never hands the next hop a
        // number it got from a stranger. Outside the signature by
        // necessity — see [`MAX_TTL`] for why the answer is to clamp it
        // rather than to refuse the event.
        event.ttl = event.ttl.min(MAX_TTL);
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

    /// The event log this service replicates through.
    ///
    /// Exposed so the node can sweep it on a timer — see
    /// `EventStore::prune_before` for why an unbounded recovery buffer is
    /// the largest thing on a busy node's disk.
    pub fn store(&self) -> &EventStore<S> {
        &self.store
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

    /// The public key an origin's `PeerId` *is*, with the out-of-band map
    /// as a fallback.
    ///
    /// A libp2p Ed25519 peer id is the size-inline multihash of the
    /// protobuf-encoded public key — the key is carried in the identifier,
    /// not hashed away (see `openfiat_network::identity`). Deriving it
    /// here means the origin→key binding is a fact about the envelope
    /// rather than something this node was told, so there is no
    /// registration an attacker could win a race on and no key this node
    /// can be missing for an origin several hops away.
    fn origin_key(&self, origin: &PeerId) -> Option<PublicKey> {
        Libp2pPeerId::from_bytes(origin.as_bytes())
            .ok()
            .and_then(public_key_from_peer_id)
            .or_else(|| self.peer_keys.get(origin).copied())
    }

    /// §9 local validation, applied identically to received events.
    ///
    /// Four questions, in the order that makes the cheap ones cheap:
    /// is this the protocol version we speak, is the event stamped
    /// somewhere a clock could plausibly be, did the identity it names
    /// actually sign it, and is its id the id its own content computes
    /// to. The last is what stops a valid signature from being reusable
    /// as raw material — see [`GossipError::EventIdMismatch`].
    ///
    /// Full "event authorization" for a *remote* origin (was this peer
    /// actually allowed to emit this event type?) needs the Service
    /// Registry to know what roles a remote `PeerId` holds —
    /// [`authorization::is_authorized`] is applied at local origination
    /// only, and `docs/dishonest-node.md` says plainly what that leaves
    /// open.
    fn validate(&self, event: &EventEnvelope) -> Result<(), GossipError> {
        if event.version != 1 {
            return Err(GossipError::ProtocolVersionMismatch);
        }
        if event.timestamp.as_millis()
            > Timestamp::now()
                .as_millis()
                .saturating_add(MAX_CLOCK_SKEW_MILLIS)
        {
            return Err(GossipError::TimestampTooFarAhead);
        }
        let public_key = self
            .origin_key(&event.origin)
            .ok_or(GossipError::InvalidSignature)?;
        let signable = event_id::signable_bytes(
            &event.event_type,
            event.ofs_spec,
            &event.origin,
            event.timestamp,
            &event.payload,
        );
        verify(&public_key, &signable, &event.signature)
            .map_err(|_| GossipError::InvalidSignature)?;
        if !event_id::matches(event) {
            return Err(GossipError::EventIdMismatch);
        }
        Ok(())
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

    /// One swarm event, when this service is the only thing on the swarm.
    ///
    /// A node that also runs peer discovery routes instead — see
    /// [`Self::handle_lifecycle`] and [`Self::handle_message`], which this
    /// is written in terms of so the two paths cannot drift.
    pub fn handle(&mut self, event: SwarmEvent<OpenFiatBehaviourEvent>) {
        if let SwarmEvent::Behaviour(OpenFiatBehaviourEvent::Envelope(
            request_response::Event::Message { peer, message, .. },
        )) = event
        {
            self.handle_message(peer, message);
            return;
        }
        self.handle_lifecycle(&event);
    }

    /// Connection, listen-address and identify events.
    ///
    /// By reference, because every service sharing this swarm needs them.
    /// Only envelope messages have a single owner.
    pub fn handle_lifecycle(&mut self, event: &SwarmEvent<OpenFiatBehaviourEvent>) {
        match event {
            SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                let peer_id = *peer_id;
                self.connected.insert(peer_id);
                // No key is cached here any more. `origin_key` recovers
                // the signing key from whichever `PeerId` an envelope
                // actually names, which covers this peer and every origin
                // behind it; caching one entry per connection covered
                // only the first of those and grew by one row for every
                // identity anyone cared to dial us from.
                self.request_recovery(peer_id);
            }
            SwarmEvent::ConnectionClosed { peer_id, .. } => {
                self.connected.remove(peer_id);
                self.recovery_served.remove(peer_id);
            }
            // What libp2p actually bound, one event per interface once a
            // wildcard is expanded. A local fact — nobody is being taken
            // at their word.
            SwarmEvent::NewListenAddr { address, .. } => {
                if is_dialable(address) {
                    self.bound.insert(address.clone());
                }
                self.record_reachable(address.clone());
            }
            // What a peer saw. The only source that can see through NAT,
            // and a claim rather than an observation: a peer can report
            // anything at all. Recorded against the peer that reported it,
            // so `corroborated_addresses` can require more than one — see
            // there for what a single reporter could otherwise do.
            SwarmEvent::Behaviour(OpenFiatBehaviourEvent::Identify(
                libp2p::identify::Event::Received { peer_id, info, .. },
            )) => {
                self.record_observed(*peer_id, info.observed_addr.clone());
            }
            _ => {}
        }
    }

    /// One gossip envelope. The caller has already established it is not
    /// another protocol's — see `openfiat_discovery::DiscoveryService::owns`.
    pub fn handle_message(&mut self, peer: Libp2pPeerId, message: Message<Envelope, Envelope>) {
        match message {
            Message::Request {
                request, channel, ..
            } => self.on_request(peer, request, channel),
            Message::Response { response, .. } => self.on_response(peer, response),
        }
    }

    /// Records `peer`'s claim to have seen this node at `address`.
    ///
    /// The claim is kept against its reporter and never merged into a
    /// count, so a peer that reconnects a hundred times still counts once.
    ///
    /// Every byte of this comes from a peer: identify's `observed_addr` is
    /// a free-text field on the far side of the connection. So both
    /// dimensions are capped — how many addresses are remembered at all
    /// ([`MAX_REACHABLE_ADDRESSES`]) and how many reporters are remembered
    /// per address ([`MAX_OBSERVERS_PER_ADDRESS`]) — because the question
    /// being answered needs [`MIN_OBSERVERS`] of them and the supply is a
    /// stranger's imagination.
    fn record_observed(&mut self, peer: Libp2pPeerId, address: Multiaddr) {
        // Deliberately gated on `reachable`, not on `observed_by`'s own
        // size: an address that was too late to be remembered as reachable
        // must not accumulate reporters either, or the cap moves the
        // growth one map to the left.
        if is_dialable(&address) && self.record_reachable(address.clone()) {
            let reporters = self.observed_by.entry(address).or_default();
            if reporters.len() < MAX_OBSERVERS_PER_ADDRESS {
                reporters.insert(peer);
            }
        }
    }

    /// Whether `address` is one this node is now tracking — either just
    /// added, or already known. `false` means it was refused: undialable,
    /// or past [`MAX_REACHABLE_ADDRESSES`].
    fn record_reachable(&mut self, address: Multiaddr) -> bool {
        if !is_dialable(&address) {
            return false;
        }
        if self.reachable.contains(&address) {
            return true;
        }
        if self.reachable.len() >= MAX_REACHABLE_ADDRESSES {
            return false;
        }
        self.reachable.insert(address.clone());
        self.newly_reachable.push(address);
        true
    }

    /// Every address this node has been told it is reachable at, from
    /// either source. For showing an operator, not for deciding anything —
    /// see [`corroborated_addresses`](Self::corroborated_addresses).
    pub fn reachable_addresses(&self) -> Vec<Multiaddr> {
        self.reachable.iter().cloned().collect()
    }

    /// The addresses this node is willing to *act* on: what libp2p bound
    /// locally, plus any address at least [`MIN_OBSERVERS`] distinct peers
    /// independently reported seeing this node at.
    ///
    /// The corroboration requirement exists because `observed_addr` is a
    /// claim by one peer and this node publishes what it concludes from it
    /// — `openfiat_snapshot` derives the URL it tells the whole cluster to
    /// fetch its snapshots from. A single peer reporting
    /// `observed_addr: <someone else's address>` would otherwise aim every
    /// bootstrapping node in the network at a third party, using an honest
    /// producer's signature to do it. Two unrelated peers reporting the
    /// same address is not proof, but it costs an attacker a second
    /// identity that must also be connected, and it is what the address is
    /// worth: a hint that verified bytes are then checked against.
    ///
    /// The cost is a node that is behind NAT *and* has only ever had one
    /// peer, which learns no public address here. That node states one
    /// with `--external-addr` or `--snapshot-public-url`, exactly as it
    /// did before anything was derived at all.
    pub fn corroborated_addresses(&self) -> Vec<Multiaddr> {
        self.bound
            .iter()
            .cloned()
            .chain(
                self.observed_by
                    .iter()
                    .filter(|(_, reporters)| reporters.len() >= MIN_OBSERVERS)
                    .map(|(address, _)| address.clone()),
            )
            .collect()
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
                // Acknowledged even though nothing reads the answer.
                //
                // A push is fire-and-forget at the protocol level, and an
                // earlier version dropped `channel` here on the reasoning
                // that this was "a valid, harmless outcome". It is valid
                // and it is not harmless: the transport underneath is
                // request-response, where an unanswered inbound request
                // holds a stream slot until it times out. Under a burst —
                // a peer connecting and forwarding its backlog — the
                // receiver exhausts its inbound capacity and starts
                // dropping streams, which is `Dropping inbound stream
                // because we are at capacity` on a live node. Answering
                // at once releases the slot and costs an empty envelope.
                self.acknowledge(channel);
            }
            MESSAGE_TYPE_RECOVERY_REQUEST => {
                let events = match wire::from_bytes::<RecoveryRequest>(&envelope.payload) {
                    // Once per connection. A second ask is answered with
                    // nothing rather than ignored: the answer is what
                    // frees the sender's inbound stream slot, and a
                    // request left unanswered is the resource leak
                    // `MESSAGE_TYPE_PUSH_ACK` exists to avoid.
                    Ok(request) if self.recovery_served.insert(peer) => {
                        self.recoverable_for(&request.subscription)
                    }
                    // Undecodable payloads land here too, for the same
                    // reason: a peer that can make this node hold a
                    // stream open by sending garbage has found a cheaper
                    // flood than sending anything real.
                    _ => Vec::new(),
                };
                let payload = wire::to_bytes(&RecoveryResponse { events })
                    .expect("RecoveryResponse always serializes");
                let response = Envelope::new(OFS_SPEC, MESSAGE_TYPE_RECOVERY_RESPONSE, 1, payload);
                let _ = self
                    .node
                    .swarm
                    .behaviour_mut()
                    .envelope
                    .send_response(channel, response);
            }
            // A message type this node does not implement is still an
            // inbound request holding a stream slot. Dropping the channel
            // costs the sender nothing and costs this node a slot until
            // the timeout, which is a flood anyone can mount with a typo.
            _ => self.acknowledge(channel),
        }
    }

    /// As much of the event log as one recovery response can carry,
    /// oldest first.
    ///
    /// Oldest first because the requester is filling a gap it has already
    /// fallen behind: giving it the front of the gap lets it make
    /// contiguous progress, and anything new arrives by push anyway now
    /// that it is connected. A peer whose gap is wider than one response
    /// does not converge from this log at all and needs a snapshot —
    /// which is the same boundary `EventStore::prune_before` already
    /// draws, reached for a different reason.
    fn recoverable_for(&self, subscription: &Subscription) -> Vec<EventEnvelope> {
        let mut events = self.store.all_for_subscription(subscription);
        events.sort_by_key(|event| event.timestamp);

        let mut budget = RECOVERY_RESPONSE_BUDGET_BYTES;
        let mut fits = Vec::new();
        for event in events {
            let size = wire::to_bytes(&event).map(|bytes| bytes.len()).unwrap_or(0);
            let Some(remaining) = budget.checked_sub(size) else {
                break;
            };
            budget = remaining;
            fits.push(event);
        }
        fits
    }

    /// Returns the empty ack that frees the sender's inbound stream slot.
    fn acknowledge(&mut self, channel: ResponseChannel<Envelope>) {
        let ack = Envelope::new(OFS_SPEC, MESSAGE_TYPE_PUSH_ACK, 1, Vec::new());
        let _ = self
            .node
            .swarm
            .behaviour_mut()
            .envelope
            .send_response(channel, ack);
    }

    fn on_response(&mut self, peer: Libp2pPeerId, envelope: Envelope) {
        if envelope.header.message_type == MESSAGE_TYPE_RECOVERY_RESPONSE
            && let Ok(response) = wire::from_bytes::<RecoveryResponse>(&envelope.payload)
        {
            for event in response.events {
                // `Some(peer)`, and that is the whole fix. Passing `None`
                // here — as this did — excluded nobody from the
                // re-broadcast, so every recovered event went straight
                // back to the node that had just supplied it. Both sides
                // request recovery the moment they connect, so two nodes
                // meeting handed each other their entire backlogs and
                // then immediately pushed them back: 174 `Dropping
                // inbound stream because we are at capacity` warnings in
                // three minutes, measured on a real pair, and every
                // dropped stream is an event that has to be fetched again.
                self.receive_event(Some(peer), event);
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

#[cfg(test)]
/// What this node is willing to believe about its own address, and from
/// how many independent mouths.
mod corroboration {
    use super::*;
    use openfiat_storage::mem::MemoryStore;

    fn service() -> GossipService<MemoryStore> {
        let keypair = Keypair::from_seed([31u8; 32]);
        GossipService::new(
            openfiat_network::Node::new(&keypair).unwrap(),
            EventStore::new(MemoryStore::new()),
            Keypair::from_seed(keypair.seed()),
            vec![NodeRole::MerchantGateway],
            Subscription::All,
        )
    }

    fn address(raw: &str) -> Multiaddr {
        raw.parse().unwrap()
    }

    /// The vector this closes. One peer says "I saw you at
    /// `<a stranger's address>`", and `openfiat_snapshot` would sign that
    /// into an announcement telling every joining node in the cluster to
    /// fetch from there.
    #[test]
    fn one_peer_alone_cannot_decide_where_this_node_is() {
        let mut service = service();
        service.record_observed(Libp2pPeerId::random(), address("/ip4/203.0.113.9/tcp/4001"));

        assert!(
            service.corroborated_addresses().is_empty(),
            "a single unverified claim must not become an address this node acts on"
        );
        assert_eq!(
            service.reachable_addresses().len(),
            1,
            "it is still worth showing an operator — it just decides nothing"
        );
    }

    #[test]
    fn two_independent_peers_reporting_the_same_address_is_enough() {
        let mut service = service();
        let observed = address("/ip4/203.0.113.9/tcp/4001");
        service.record_observed(Libp2pPeerId::random(), observed.clone());
        service.record_observed(Libp2pPeerId::random(), observed.clone());

        assert_eq!(service.corroborated_addresses(), vec![observed]);
    }

    /// Counting reports rather than reporters would let one peer
    /// corroborate itself by reconnecting.
    #[test]
    fn one_peer_repeating_itself_is_still_one_peer() {
        let mut service = service();
        let peer = Libp2pPeerId::random();
        let observed = address("/ip4/203.0.113.9/tcp/4001");
        for _ in 0..50 {
            service.record_observed(peer, observed.clone());
        }

        assert!(service.corroborated_addresses().is_empty());
    }

    /// A bound address is a local fact, so it needs nobody's agreement —
    /// which is what keeps an ordinary public-interface node working with
    /// no peers at all.
    #[test]
    fn a_bound_address_needs_no_corroboration() {
        let mut service = service();
        let bound = address("/ip4/198.51.100.4/udp/4001/quic-v1");
        service.bound.insert(bound.clone());

        assert_eq!(service.corroborated_addresses(), vec![bound]);
    }
}
