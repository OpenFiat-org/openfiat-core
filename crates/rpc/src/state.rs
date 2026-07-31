//! `NodeState<S>` composes every domain registry this workspace has
//! built, all sharing one physical `S` (via `Rc<S>`'s `KvStore` impl —
//! see `openfiat-storage`) the same way a real node backs everything
//! with a single RocksDB `Database`, over one real, gossip-connected
//! [`GossipService`] — the same composition `openfiat-conformance::
//! FullNode` uses (every registry's `apply_event` attached via
//! `add_event_handler`), evolved into the actual shipped node rather
//! than staying a test-only harness. Constructed once, inside the actor
//! thread — see the `actor` module doc for why it can never cross a
//! thread boundary.
//!
//! `gossip` is `RefCell`-wrapped because every dispatch handler only
//! borrows `&NodeState<S>` (see `dispatch::MethodFn`) — matching every
//! other registry here, which already hides its own mutability behind
//! `&self` methods.

use openfiat_advertisements::AdvertisementRegistry;
use openfiat_chain::events::BlockhashAnnounced;
use openfiat_chain::{ChainBridge, ChainState, NodeChainMode};
use openfiat_content::{AttachmentRegistry, HeldContent};
use openfiat_crypto::Keypair;
use openfiat_crypto::challenge::ChallengeLedger;
use openfiat_discovery::DiscoveryService;
use openfiat_discovery::cache::PeerCache;
use openfiat_disputes::DisputeRegistry;
use openfiat_gossip::{EventStore, GossipService, Subscription};
use openfiat_governance::GovernanceRegistry;
use openfiat_identity::IdentityRegistry;
use openfiat_network::Node;
use openfiat_notifications::NotificationRegistry;
use openfiat_notifications::routing::PlannedDelivery;
use openfiat_oracles::OracleIndex;
use openfiat_registry::Registry as ServiceRegistry;
use openfiat_registry::earnings::EarningsLedger;
use openfiat_reputation::ReputationView;
use openfiat_reservations::ReservationRegistry;
use openfiat_reviews::{ReviewRegistry, ReviewsView};
use openfiat_rewards::{LivenessLedger, RewardParams};
use openfiat_risk::RiskIndex;
use openfiat_serialization::wire;
use openfiat_sessions::SessionRegistry;
use openfiat_settlement::SettlementRegistry;
use openfiat_snapshot::SnapshotIndex;
use openfiat_storage::KvStore;
use openfiat_trade::{CounterpartyView, TradeView};
use openfiat_types::NodeRole;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

/// A gossiped or locally-submitted `VoteCast` (OFS-4000) awaiting real
/// on-chain verification of the stake it claims as its weight — see
/// `actor::poll_vote_verifications` for what drains this and
/// `onchain_stake` for how the claim is checked. `signed_vote_bytes` is
/// the vote's own wire-serialized `SignedVoteCast`, kept opaque here so
/// this crate's composition root doesn't need a governance-specific
/// type to hold it in a queue.
pub struct PendingVoteVerification {
    pub stake_account: String,
    pub signed_vote_bytes: Vec<u8>,
    /// How many times this claim has already been looked up without a
    /// usable answer (account not yet visible, or a transient RPC error).
    /// Bounded by `actor::VOTE_VERIFICATION_MAX_ATTEMPTS` so an account
    /// that will never exist cannot be retried forever — an unbounded
    /// retry is how a queue silently grows and a node quietly stops
    /// finishing governance verification.
    pub attempts: u32,
}

/// Every column family that makes up this node's replicated worldview,
/// and therefore everything a snapshot of it must carry.
///
/// Defined here, in the one module that composes all of them, rather than
/// in `openfiat-snapshot` (which deliberately knows no domain crate) or in
/// `openfiat-node` (where a new registry could be added to the database's
/// open list and silently left out of every snapshot). A domain crate
/// joining `NodeState` above and not appearing here would ship snapshots
/// missing its state.
///
/// Three column families are deliberately absent:
/// - `gossip_events` — the event log a snapshot exists to spare a joining
///   node from replaying. Including it would make a snapshot larger than
///   the history it replaces.
/// - `snapshot_metadata` and `snapshot_checkpoint` — node-local snapshot
///   bookkeeping, which `openfiat_snapshot::store::RESERVED_COLUMN_FAMILIES`
///   also refuses to let an import overwrite.
///
/// # `pinned_content` is in the list, and used not to be
///
/// It was excluded on the grounds that it is bulk data and "a peer can
/// re-fetch it from IPFS". That reasoning treats availability as somebody
/// else's problem, which is exactly what fails when an uploader stops
/// paying their pinning service — the failure this network's content
/// premium exists to prevent. A node bootstrapping from a snapshot came
/// up able to answer records and unable to serve a single byte of the
/// evidence they reference, freeloading for hours while it refetched
/// content its peers already had.
///
/// Size is real now rather than trivial, and two things bound it. The
/// column family *is* the node's retention window — eviction sweeps it
/// every pinning tick — so an archival node ships everything and a
/// rolling node ships its window, without a second policy to keep in
/// step. And `openfiat_snapshot::codec::MAX_SNAPSHOT_BYTES` is the
/// ceiling on the whole blob, which a producer hits before its peers do.
///
/// The import side is not optional: see [`verify_snapshot_entry`]. A
/// block that does not hash to its key is corruption or an attempt to
/// make this node serve someone else's bytes under a trusted CID, and the
/// state root does not catch it — the state root proves the blob is what
/// the producer announced, not that the producer filled it honestly.
pub const SNAPSHOT_COLUMN_FAMILIES: &[&str] = &[
    "advertisements",
    "reservations",
    "settlements",
    "disputes",
    "identity_claims",
    "attachments",
    "governance_proposals",
    "registry_services",
    "notification_subscriptions",
    "notification_receipts",
    "notification_dispatches",
    "oracle_records",
    "risk_records",
    "sessions",
    openfiat_content::CONTENT_COLUMN_FAMILY,
    openfiat_reviews::REVIEWS_COLUMN_FAMILY,
];

/// What this node will accept into each column family of a snapshot.
///
/// Records pass: an entry in `settlements` is whatever the network says
/// it is, and a producer able to forge one could have gossiped it
/// instead, so refusing it here would buy nothing.
///
/// Content blocks are checked, because they are the one thing in this
/// store that an importer can judge for itself. A CID *is* the hash of
/// the block it names, so a key/value pair either agrees with itself or
/// does not — and one that does not is either corruption or a producer
/// arranging for this node to serve their bytes under an identifier
/// somebody else's signed record points at. This node would then hand
/// those bytes to a challenger and to any browser that asked.
///
/// A snapshot's state root cannot substitute for this. It proves the blob
/// is the one the producer announced and signed; the producer computed it
/// over whatever they assembled.
pub fn verify_snapshot_entry(column_family: &str, key: &[u8], value: &[u8]) -> bool {
    if column_family != openfiat_content::CONTENT_COLUMN_FAMILY {
        return true;
    }
    let Ok(spelling) = std::str::from_utf8(key) else {
        return false;
    };
    let Ok(cid) = openfiat_crypto::Cid::parse(spelling) else {
        return false;
    };
    // The same two conditions `HeldContent::keep` applies to a block
    // arriving from a peer, because a snapshot is a peer's bytes too.
    value.len() <= openfiat_content::MAX_BLOCK_BYTES && cid.matches(value)
}

/// The OFS specifications this node implements, as it tells peers.
///
/// Defined here for the same reason `SNAPSHOT_COLUMN_FAMILIES` is: this
/// module is the one place that composes every domain crate, so it is the
/// only place that can honestly answer "what does this node speak". A
/// crate joining `NodeState` and not appearing here would be a capability
/// the node has and never advertises — and one appearing here without its
/// crate would be a claim it cannot honour, which is worse.
pub const SUPPORTED_OFS: &[u16] = &[
    1000, // Network transport
    1100, // Peer discovery
    1200, // Gossip
    1300, // Snapshot synchronisation
    1400, // Session synchronisation
    1500, // Service registry
    2000, // Trade
    2100, // Advertisements
    2200, // Reservations
    2300, // Settlement
    2400, // Disputes
    3000, // Reputation
    4000, // Governance
    4300, // Chain bridge
    6000, // Notifications
    7000, // Oracles
    8200, // JSON-RPC / HTTP API
];

/// How many peers a node dials of its own accord.
///
/// A ceiling on discovery's appetite, not a target to reach: a node under
/// it dials newly-learned peers, and one over it caches them without
/// dialling. Every connection costs a file descriptor and a heartbeat, so
/// an unbounded version turns a large network into a node that spends its
/// time maintaining connections rather than using them.
const DISCOVERY_TARGET_PEERS: usize = 32;

pub struct NodeState<S> {
    pub gossip: RefCell<GossipService<Rc<S>>>,
    /// Peer discovery (OFS-1100), running on the same swarm `gossip` owns.
    ///
    /// It is a separate `RefCell` rather than a field of `GossipService`
    /// because the two are peers, not layers: gossip replicates events,
    /// discovery finds who to replicate with, and neither is built on the
    /// other. What they share is one connection — which is the whole point
    /// of OFNP §20's multiplexing, and why `actor::drive_network` routes
    /// each envelope by its OFS spec number rather than either service
    /// reaching into the other.
    ///
    /// This crate did not depend on `openfiat-discovery` at all until now.
    /// The service was fully implemented, tested to five-node convergence,
    /// and constructed by nothing — so a running node announced no address
    /// and learned no peer it was not handed statically.
    pub discovery: RefCell<DiscoveryService<Rc<S>>>,
    /// The one physical store every registry above writes through — held
    /// so this node can snapshot its own state (see
    /// `actor::poll_snapshot_production`). Every other reader goes
    /// through a registry; nothing should reach past one to this.
    pub store: Rc<S>,
    pub advertisements: Rc<AdvertisementRegistry<Rc<S>>>,
    pub reservations: Rc<ReservationRegistry<Rc<S>>>,
    pub settlements: Rc<SettlementRegistry<Rc<S>>>,
    pub trades: TradeView<Rc<S>>,
    /// One wallet's own trading history folded per counterparty — the
    /// data behind "you have traded 6 times with this wallet". Reachable
    /// only through `methods::counterparties`, which will not answer for
    /// a wallet the caller cannot prove they control; see that module
    /// for why this one aggregate is gated when no other read is.
    pub counterparties: CounterpartyView<Rc<S>>,
    /// The outstanding wallet-ownership challenges guarding every gated
    /// read: counterparty history, and a party's own settlements,
    /// reservations and disputes (`methods::wallet_auth`).
    ///
    /// One ledger for all of them. A nonce is worthless without the key
    /// that signs it, and each surface signs under its own domain
    /// separator, so a signature collected for one cannot be presented on
    /// another — sharing the ledger costs nothing and a second one would
    /// be a second thing to expire correctly.
    ///
    /// In memory only, deliberately: persisting them would leave a node
    /// operator a record of who asked about whom, which is the exact
    /// trail these features exist to avoid creating.
    pub wallet_challenges: Rc<RefCell<ChallengeLedger>>,
    pub disputes: Rc<DisputeRegistry<Rc<S>>>,
    pub identity: Rc<IdentityRegistry<Rc<S>>>,
    /// Trade evidence, addressed by IPFS CID. Never the bytes — see
    /// `openfiat_content` for why a node stores the reference and not the
    /// file, and why reading a settlement's attachments requires being
    /// told who its parties are.
    pub attachments: Rc<AttachmentRegistry<Rc<S>>>,
    /// Content this node keeps a local copy of, so it can answer a
    /// retrievability challenge synchronously and serve the block to any
    /// IPFS peer that asks. See `openfiat_content::held` for why the copy
    /// exists and why 256 KiB bounds a block rather than a file.
    pub held_content: Rc<HeldContent<Rc<S>>>,
    /// CIDs this node has asked its peers for and not yet received.
    ///
    /// The gate on what an inbound bitswap message may cause this node to
    /// store. Blocks are verified against their CID before being kept, so
    /// an unsolicited one cannot be *wrong* — but nothing stops a peer
    /// pushing correct blocks for content this node never wanted until
    /// the disk is full. Only what is on this list is kept.
    ///
    /// In memory, not persisted: it is derived from the attachment
    /// records on every pinning tick, so a restart rebuilds it from the
    /// records rather than trusting a stale copy.
    pub content_wants: RefCell<std::collections::HashSet<String>>,
    /// Content handed to this node through `sendContentPut`, and when.
    ///
    /// The retention sweep keeps what the *records* reference, which is
    /// the right rule and arrives in the wrong order: an interface
    /// uploads an avatar's blocks and only then publishes the claim that
    /// names them. Without this, a sweep landing in between evicts the
    /// bytes and the claim points at content the node just threw away —
    /// and since a sweep runs at startup, "in between" is not a narrow
    /// window.
    ///
    /// So an upload is held for [`crate::actor::INGRESS_GRACE`] whether
    /// or not anything references it yet, and after that the ordinary
    /// rule resumes. Bounded on purpose: the ingress is open, so this is
    /// exactly how much disk a stranger can occupy without publishing a
    /// record, and it must stay a window rather than become a promise.
    ///
    /// In memory, like `content_wants`. A restart forgets the grace and
    /// the next sweep applies the records' rule, which is the honest
    /// answer — nothing on disk records an intention that was never
    /// published.
    pub content_ingress: RefCell<std::collections::HashMap<String, openfiat_types::Timestamp>>,
    /// The IPFS multihashes this node has announced itself as a provider
    /// of, so a running announcement is renewed by libp2p rather than
    /// re-issued on every tick.
    ///
    /// Multihashes rather than CIDs, because that is what the DHT is
    /// keyed by — see `openfiat_crypto::Cid::multihash`. Held as the
    /// CID's own spelling for readability and converted at the boundary,
    /// since one multihash has exactly one canonical CID per codec and
    /// this node only ever announces content it holds.
    ///
    /// In memory, like `content_wants`: a restarted node re-announces
    /// from its records, which is also what re-establishes records the
    /// network expired while it was down.
    pub content_provided: RefCell<std::collections::HashSet<String>>,
    /// The identity-conflict count already reported, so a cloned wallet
    /// is announced when the count changes rather than on every tick.
    pub reported_identity_conflicts: std::cell::Cell<u64>,
    pub reputation: ReputationView<Rc<S>>,
    /// Post-trade reviews as published — the write path and the gossip
    /// handler. Never a read path for anything a client sees: a stored
    /// review is an unattributed claim until a settlement says who was
    /// entitled to make it, which is `reviews_view`'s job.
    pub reviews: Rc<ReviewRegistry<Rc<S>>>,
    /// The same reviews joined against the settlements that authorize
    /// them. Everything `methods::reputation` hands out comes from here,
    /// and it is also where the public/party split lives — see
    /// `openfiat_reviews::view` on why a public review names its subject
    /// and neither its author nor its trade.
    pub reviews_view: ReviewsView<Rc<S>>,
    pub governance: Rc<GovernanceRegistry<Rc<S>>>,
    pub services: Rc<ServiceRegistry<Rc<S>>>,
    /// What each registered service has earned, and the outstanding
    /// single-use challenges guarding reads of it (OFS-4100 §9.5).
    /// Nothing credits it yet — the billing trigger differs by role and
    /// is deliberately unsettled — so every statement currently reads
    /// empty. See `openfiat_registry::earnings`.
    pub provider_earnings: Rc<RefCell<EarningsLedger>>,
    pub notifications: Rc<NotificationRegistry<Rc<S>>>,
    pub oracles: Rc<OracleIndex<Rc<S>>>,
    pub risk: Rc<RiskIndex<Rc<S>>>,
    pub snapshots: Rc<SnapshotIndex<Rc<S>>>,
    pub sessions: Rc<SessionRegistry<Rc<S>>>,
    pub chain: Rc<ChainState>,
    /// Per-epoch liveness observations feeding OFS-4100 §9.2's node
    /// reward share, recorded from every signed envelope this node
    /// receives. Local by construction — see `openfiat_rewards::liveness`
    /// on why a node can only honestly speak to what it heard itself.
    pub reward_observations: Rc<RefCell<LivenessLedger>>,
    /// The reward parameters this node measures against. Held here rather
    /// than read at each use so a schedule and the observations behind it
    /// can never be computed under two different epoch lengths.
    pub reward_params: RewardParams,
    /// The Chain Bridge's gossip-facing half (OFS-4300 §6-7) — installed
    /// on the same shared `gossip` above, alongside every registry's
    /// `apply_event`. Kept separate from `chain` (which is what
    /// `rpc::methods::chain`'s synchronous handlers read/write) because
    /// only this half needs `&mut GossipService` to originate anything;
    /// see `new`'s wiring of the two together.
    pub chain_bridge: ChainBridge,
    /// Governance `VoteCast`s (this node's own `sendVoteCast` submissions
    /// and every peer's gossiped ones alike — see `new`'s governance
    /// event-handler wiring) awaiting independent on-chain stake
    /// verification before any weight is trusted. Drained by
    /// `actor::poll_vote_verifications`.
    pending_vote_verifications: Rc<RefCell<VecDeque<PendingVoteVerification>>>,
    /// Notifications this node has planned but not yet handed to a
    /// gateway (OFS-6000). Queued synchronously by `notify`'s gossip
    /// handler — which must never do I/O, or it would stall the event
    /// loop — and drained by `actor::poll_notifications`, which owns the
    /// HTTP hop. Same shape, and the same reasoning, as
    /// `pending_vote_verifications`.
    pending_notifications: Rc<RefCell<VecDeque<PlannedDelivery>>>,
    /// Turns observed protocol events into those planned notifications.
    /// Held so `actor::poll_chain` can also report the one trigger that
    /// has no gossip event of its own (`EscrowReleased`).
    pub notification_dispatcher: Rc<crate::notify::NotificationDispatcher<Rc<S>>>,
}

impl<S: KvStore + 'static> NodeState<S> {
    /// `node` is this process's real libp2p transport (already bound to
    /// an identity keypair — see `openfiat_network::Node::new`);
    /// `keypair` is the same identity, reused here to sign gossip
    /// envelopes; `self_roles` gates which event types this node may
    /// originate (`openfiat_gossip::authorization`); `chain_mode`
    /// decides whether `rpc::methods::chain`'s handlers behave as
    /// `RpcConnected` or `GossipOnly` (OFS-4300 §4) — actually acting on
    /// `RpcConnected` mode (polling a real Solana RPC endpoint) is the
    /// async node-composition layer's job (`actor::spawn_actor`), not
    /// this constructor's.
    pub fn new(
        node: Node,
        store: S,
        keypair: Keypair,
        self_roles: Vec<NodeRole>,
        chain_mode: NodeChainMode,
        trusted_snapshot_providers: openfiat_snapshot::trust::TrustAnchors,
    ) -> Self {
        let store = Rc::new(store);
        let event_store = EventStore::new(Rc::clone(&store));
        // Built before the swarm is handed to gossip, because the service
        // needs this node's own peer id and the swarm is what knows it.
        let discovery = DiscoveryService::new(
            node.local_peer_id(),
            PeerCache::new(Rc::clone(&store)),
            keypair.public_key(),
            openfiat_network::behaviour::AGENT_VERSION.to_string(),
            SUPPORTED_OFS.to_vec(),
            self_roles.clone(),
            DISCOVERY_TARGET_PEERS,
        );
        let mut gossip =
            GossipService::new(node, event_store, keypair, self_roles, Subscription::All);

        let services = Rc::new(ServiceRegistry::new(Rc::clone(&store)));
        let provider_earnings = Rc::new(RefCell::new(EarningsLedger::new()));
        let advertisements = Rc::new(AdvertisementRegistry::new(Rc::clone(&store)));
        let reservations = Rc::new(ReservationRegistry::new(
            Rc::clone(&store),
            Rc::clone(&advertisements),
        ));
        let settlements = Rc::new(SettlementRegistry::new(Rc::clone(&store)));
        let disputes = Rc::new(DisputeRegistry::new(
            Rc::clone(&store),
            Rc::clone(&settlements),
        ));
        let trades = TradeView::new(Rc::clone(&reservations), Rc::clone(&settlements));
        let counterparties = CounterpartyView::new(Rc::clone(&settlements), Rc::clone(&disputes));
        let reputation = ReputationView::new(
            Rc::clone(&reservations),
            Rc::clone(&settlements),
            Rc::clone(&disputes),
        );
        let reviews = Rc::new(ReviewRegistry::new(Rc::clone(&store)));
        let reviews_view = ReviewsView::new(Rc::clone(&reviews), Rc::clone(&settlements));
        let identity = Rc::new(IdentityRegistry::new(Rc::clone(&store)));
        let attachments = Rc::new(AttachmentRegistry::new(Rc::clone(&store)));
        let held_content = Rc::new(HeldContent::new(Rc::clone(&store)));
        let governance = Rc::new(GovernanceRegistry::new(Rc::clone(&store)));
        let notifications = Rc::new(NotificationRegistry::new(
            Rc::clone(&store),
            Rc::clone(&services),
        ));
        let oracles = Rc::new(OracleIndex::new(Rc::clone(&store), Rc::clone(&services)));
        let risk = Rc::new(RiskIndex::new(Rc::clone(&store), Rc::clone(&services)));
        let snapshots = Rc::new(SnapshotIndex::with_anchors(
            Rc::clone(&store),
            Rc::clone(&services),
            trusted_snapshot_providers,
            verify_snapshot_entry,
        ));
        let sessions = Rc::new(SessionRegistry::new(Rc::clone(&store)));
        let is_rpc_connected = chain_mode.is_rpc_connected();
        let chain = Rc::new(ChainState::new(chain_mode));
        let chain_bridge = ChainBridge::install(&mut gossip);

        // Keeps `chain`'s cache (what `getChainStatus`/`getLatestBlockhash`
        // actually read) in sync with every `BlockhashAnnounced` this
        // node stores — its own self-announcement (an `RpcConnected`
        // node's poll loop) or a peer's (a `GossipOnly` node's only
        // source) alike, so both modes answer those two methods
        // identically per OFS-4300 §8's own requirement.
        // Every signed envelope is a liveness datapoint for whoever
        // originated it (OFS-4100 §9.2). Installed ahead of the
        // domain handlers and matching no event type in particular:
        // presence is the signal, not what the peer happened to say.
        let reward_observations: Rc<RefCell<LivenessLedger>> =
            Rc::new(RefCell::new(LivenessLedger::new()));
        let reward_params = RewardParams::default();
        let observations_for_gossip = Rc::clone(&reward_observations);
        gossip.add_event_handler(move |event| {
            let is_announcement = event.ofs_spec == openfiat_chain::protocol::OFS_SPEC
                && event.event_type.as_str() == openfiat_chain::protocol::EVENT_BLOCKHASH_ANNOUNCED;
            observations_for_gossip.borrow_mut().observe(
                &reward_params,
                &event.origin,
                event.timestamp,
                is_announcement,
            );
        });

        let chain_for_blockhash = Rc::clone(&chain);
        gossip.add_event_handler(move |event| {
            if event.ofs_spec == openfiat_chain::protocol::OFS_SPEC
                && event.event_type.as_str() == openfiat_chain::protocol::EVENT_BLOCKHASH_ANNOUNCED
                && let Ok(announced) = wire::from_bytes::<BlockhashAnnounced>(&event.payload)
            {
                chain_for_blockhash.record_blockhash(&announced.blockhash, announced.slot);
            }
        });

        // Only an `RpcConnected` node has anywhere to submit a peer's
        // relay request — a `GossipOnly` node registering this too would
        // just queue bytes into a `pending_relay` nothing ever drains.
        // Feeding the *same* queue `sendTransaction`'s handler already
        // uses means the actor's one poll loop submits both a caller's
        // own request and a `GossipOnly` peer's relayed one identically.
        if is_rpc_connected {
            let chain_for_relay = Rc::clone(&chain);
            chain_bridge.on_relay_requested(move |requested| {
                let _ = chain_for_relay
                    .enqueue_relay(requested.tx_bytes.clone(), requested.correlation.clone());
            });
        }

        macro_rules! attach {
            ($registry:expr) => {{
                let handler_registry = Rc::clone(&$registry);
                gossip.add_event_handler(move |event| handler_registry.apply_event(event));
            }};
        }
        attach!(advertisements);
        attach!(reservations);
        attach!(settlements);
        attach!(disputes);
        attach!(services);
        attach!(notifications);
        attach!(oracles);
        attach!(risk);
        attach!(snapshots);
        attach!(sessions);
        attach!(identity);
        attach!(attachments);
        attach!(reviews);

        // Governance's `VoteCast` is the one event type this node never
        // applies straight off the wire (self-reported or gossiped
        // alike) — its claimed weight isn't trustworthy until
        // independently checked against real on-chain stake (see
        // `PendingVoteVerification`'s own doc). Every other governance
        // event still applies directly, same as every other registry.
        let pending_vote_verifications = Rc::new(RefCell::new(VecDeque::new()));
        let governance_for_events = Rc::clone(&governance);
        let queue_for_events = Rc::clone(&pending_vote_verifications);
        gossip.add_event_handler(move |event| {
            if event.ofs_spec == openfiat_governance::protocol::OFS_SPEC
                && event.event_type.as_str() == openfiat_governance::protocol::EVENT_VOTE_CAST
            {
                if let Ok(signed) =
                    wire::from_bytes::<openfiat_governance::events::SignedVoteCast>(&event.payload)
                {
                    queue_for_events
                        .borrow_mut()
                        .push_back(PendingVoteVerification {
                            stake_account: signed.vote.stake_account.clone(),
                            signed_vote_bytes: event.payload.clone(),
                            attempts: 0,
                        });
                }
            } else {
                governance_for_events.apply_event(event);
            }
        });

        // Installed last on purpose: it reads the state every handler
        // above just wrote (a settlement's counterparties, a dispute's
        // parties) rather than re-deriving it from the payload. It only
        // ever enqueues, so nothing it does — a missing gateway, an
        // unreachable endpoint — can disturb the domain path that
        // produced the event.
        let pending_notifications: Rc<RefCell<VecDeque<PlannedDelivery>>> =
            Rc::new(RefCell::new(VecDeque::new()));
        let notification_dispatcher = Rc::new(crate::notify::NotificationDispatcher::new(
            Rc::clone(&notifications),
            Rc::clone(&advertisements),
            Rc::clone(&settlements),
            Rc::clone(&disputes),
            Rc::clone(&pending_notifications),
        ));
        let dispatcher_for_events = Rc::clone(&notification_dispatcher);
        gossip.add_event_handler(move |event| dispatcher_for_events.observe(event));

        Self {
            gossip: RefCell::new(gossip),
            store,
            pending_notifications,
            notification_dispatcher,
            advertisements,
            reservations,
            settlements,
            discovery: RefCell::new(discovery),
            trades,
            counterparties,
            wallet_challenges: Rc::new(RefCell::new(ChallengeLedger::new())),
            disputes,
            identity,
            attachments,
            held_content,
            content_wants: RefCell::new(std::collections::HashSet::new()),
            content_ingress: RefCell::new(std::collections::HashMap::new()),
            content_provided: RefCell::new(std::collections::HashSet::new()),
            reported_identity_conflicts: std::cell::Cell::new(0),
            reputation,
            reviews,
            reviews_view,
            governance,
            services,
            provider_earnings,
            notifications,
            oracles,
            risk,
            snapshots,
            sessions,
            chain,
            reward_observations,
            reward_params,
            chain_bridge,
            pending_vote_verifications,
        }
    }

    /// Every notification currently planned and awaiting its gateway
    /// handoff. Drained once per `actor::poll_notifications` tick.
    pub fn drain_notifications(&self) -> Vec<PlannedDelivery> {
        self.pending_notifications.borrow_mut().drain(..).collect()
    }

    /// Queue a planned delivery for the next handoff tick — the
    /// counterpart to `enqueue_vote_verification`, for anything that
    /// plans a notification outside the gossip handler (see
    /// `notify::NotificationDispatcher::observe_escrow_release`).
    pub fn enqueue_notification(&self, delivery: PlannedDelivery) {
        self.pending_notifications.borrow_mut().push_back(delivery);
    }

    /// Queues a governance vote for independent on-chain stake
    /// verification — see `PendingVoteVerification`'s own doc for why
    /// its weight can't just be applied here directly.
    pub fn enqueue_vote_verification(&self, stake_account: String, signed_vote_bytes: Vec<u8>) {
        self.pending_vote_verifications
            .borrow_mut()
            .push_back(PendingVoteVerification {
                stake_account,
                signed_vote_bytes,
                attempts: 0,
            });
    }

    /// Puts a drained claim back for another look, carrying its own
    /// attempt count with it. Separate from `enqueue_vote_verification`
    /// precisely so a retry cannot reset that count and loop forever —
    /// the caller (`actor::poll_vote_verifications`) is the one that
    /// decides when a claim has been retried enough, and says so out loud.
    pub fn requeue_vote_verification(&self, pending: PendingVoteVerification) {
        self.pending_vote_verifications
            .borrow_mut()
            .push_back(pending);
    }

    /// Drains every vote currently queued for verification — called once
    /// per `actor::poll_vote_verifications` tick. Anything that fails
    /// verification this round (not yet observable, or a transient RPC
    /// error) is expected to be re-queued by the caller via
    /// `requeue_vote_verification`, same retry shape as `ChainState`'s
    /// `awaiting_confirmation` — but bounded, unlike that one, because
    /// nothing else ever removes an entry that will never resolve.
    pub fn drain_vote_verifications(&self) -> Vec<PendingVoteVerification> {
        self.pending_vote_verifications
            .borrow_mut()
            .drain(..)
            .collect()
    }

    /// Test-only convenience: a lone in-process node with a throwaway
    /// identity and no role restrictions, listening on nothing. Every
    /// `NodeState` construction outside this crate's own dispatch tests
    /// needs a real transport identity; this keeps that boilerplate in
    /// one place instead of repeated at each call site.
    ///
    /// Not `cfg(test)`-gated, for the same reason `actor::NetworkConfig::
    /// for_test` is not: this crate's integration tests live under
    /// `tests/` and link against a normal build, so a gated constructor
    /// would be invisible to exactly the tests that exercise the shipped
    /// dispatch table end to end.
    /// A test node that additionally trusts `anchors` for a first
    /// snapshot.
    ///
    /// Separate from `new_for_test` on purpose. Making the plain test
    /// constructor trust everything would disable the anchor gate across
    /// the whole suite without anyone choosing that — a test asking to
    /// exercise the bootstrap pipeline should have to say so.
    pub fn new_for_test_trusting(
        store: S,
        anchors: openfiat_snapshot::trust::TrustAnchors,
    ) -> Self {
        let keypair = Keypair::generate();
        let node = Node::new(&keypair).expect("a fresh keypair always builds a node");
        Self::new(
            node,
            store,
            keypair,
            Vec::new(),
            NodeChainMode::GossipOnly,
            anchors,
        )
    }

    pub fn new_for_test(store: S) -> Self {
        let keypair = Keypair::generate();
        let node = Node::new(&keypair).expect("loopback node construction cannot fail");
        Self::new(
            node,
            store,
            keypair,
            Vec::new(),
            NodeChainMode::GossipOnly,
            openfiat_snapshot::trust::TrustAnchors::pinned(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfiat_storage::Entry;
    use openfiat_storage::mem::MemoryStore;

    /// The column families a real node's RocksDB is opened with:
    /// [`SNAPSHOT_COLUMN_FAMILIES`] plus the node-local ones `openfiat-node`
    /// adds. Spelled out here because that binary owns its own list and
    /// this crate cannot see it; only the first part is the part a new
    /// registry has to join.
    const NODE_LOCAL: &[&str] = &[
        "gossip_events",
        "snapshot_metadata",
        "snapshot_checkpoint",
        "peers",
    ];

    #[derive(Debug)]
    struct UndeclaredColumnFamily(String);

    impl std::fmt::Display for UndeclaredColumnFamily {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(
                f,
                "column family {:?} was not declared when the database was opened",
                self.0
            )
        }
    }

    impl std::error::Error for UndeclaredColumnFamily {}

    /// A store that behaves the way RocksDB does: a column family not
    /// named when the database was opened does not exist, and every
    /// access to it fails.
    ///
    /// [`MemoryStore`] creates a column family on first write, so a
    /// registry whose family nobody declared passes every in-memory test
    /// in this workspace and then silently drops every write on a real
    /// node — a bug that has already shipped here once. This wrapper is
    /// what makes that failure visible in a test.
    struct DeclaredOnly {
        inner: MemoryStore,
    }

    impl DeclaredOnly {
        fn new() -> Self {
            Self {
                inner: MemoryStore::new(),
            }
        }

        fn check(cf: &str) -> Result<(), UndeclaredColumnFamily> {
            if SNAPSHOT_COLUMN_FAMILIES.contains(&cf) || NODE_LOCAL.contains(&cf) {
                return Ok(());
            }
            Err(UndeclaredColumnFamily(cf.to_string()))
        }
    }

    impl KvStore for DeclaredOnly {
        type Error = UndeclaredColumnFamily;

        fn get(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error> {
            Self::check(cf)?;
            Ok(self.inner.get(cf, key).expect("MemoryStore is infallible"))
        }

        fn put(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), Self::Error> {
            Self::check(cf)?;
            self.inner
                .put(cf, key, value)
                .expect("MemoryStore is infallible");
            Ok(())
        }

        fn delete(&self, cf: &str, key: &[u8]) -> Result<(), Self::Error> {
            Self::check(cf)?;
            self.inner
                .delete(cf, key)
                .expect("MemoryStore is infallible");
            Ok(())
        }

        fn iter_prefix(&self, cf: &str, prefix: &[u8]) -> Result<Vec<Entry>, Self::Error> {
            Self::check(cf)?;
            Ok(self
                .inner
                .iter_prefix(cf, prefix)
                .expect("MemoryStore is infallible"))
        }
    }

    /// Drives a real write through the real registry against a store that
    /// refuses undeclared column families, which is the only way this
    /// suite can tell "the registry works" from "the registry works on a
    /// node that happens to have opened its column family".
    #[test]
    fn a_review_survives_a_store_that_only_accepts_declared_column_families() {
        use openfiat_crypto::Keypair;
        use openfiat_network::identity::peer_id_from_public_key;
        use openfiat_reviews::{Rating, Review, SignedReviewPublish};
        use openfiat_settlement::SettlementId;
        use openfiat_types::Timestamp;

        let state = NodeState::new_for_test(DeclaredOnly::new());
        let author = Keypair::generate();
        let review = Review {
            settlement: SettlementId::new("s-1"),
            author: peer_id_from_public_key(&author.public_key()).unwrap(),
            author_public_key: author.public_key(),
            rating: Rating::Five,
            comment: "paid on time".to_string(),
            created_at: Timestamp::from_millis(1),
        };

        state
            .reviews
            .apply_publish(SignedReviewPublish::sign(review, &author))
            .expect("a well-signed review is accepted");
        assert_eq!(
            state.reviews.all().len(),
            1,
            "the write went nowhere — {:?} is missing from SNAPSHOT_COLUMN_FAMILIES, so a \
             real node never opens it and every review is silently discarded",
            openfiat_reviews::REVIEWS_COLUMN_FAMILY,
        );
    }

    #[test]
    fn composes_without_panicking_and_starts_empty() {
        let state = NodeState::new_for_test(MemoryStore::new());
        assert!(state.advertisements.all().is_empty());
        assert!(state.trades.all().is_empty());
        assert!(state.services.all().is_empty());
        assert!(
            state.reward_observations.borrow().epochs_held().is_empty(),
            "a fresh node has observed nobody"
        );
    }

    /// The reward ledger is only worth anything if the node actually
    /// feeds it, so this drives a real envelope through the real gossip
    /// service rather than calling `LivenessLedger::observe` directly —
    /// the wiring is the thing under test, not the arithmetic.
    #[test]
    fn every_gossiped_event_records_liveness_for_whoever_originated_it() {
        use openfiat_types::{EventType, Priority, Timestamp};

        let state = NodeState::new_for_test(MemoryStore::new());
        let me = state.gossip.borrow().node.local_peer_id();

        state
            .gossip
            .borrow_mut()
            .originate(
                EventType::new("BlockhashAnnounced").expect("valid event type"),
                openfiat_chain::protocol::OFS_SPEC,
                Priority::SessionReservationSettlement,
                4,
                Vec::new(),
            )
            .expect("a node may originate its own chain event");

        let epoch = state.reward_params.epoch_index(Timestamp::now());
        let observed = state.reward_observations.borrow().epoch(epoch);
        let live = observed
            .get(&me)
            .expect("originating an event must register the originator as live");
        assert!(live.availability_bps(&state.reward_params) > 0);
    }
}
