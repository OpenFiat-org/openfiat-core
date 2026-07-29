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
use openfiat_crypto::Keypair;
use openfiat_disputes::DisputeRegistry;
use openfiat_gossip::{EventStore, GossipService, Subscription};
use openfiat_governance::GovernanceRegistry;
use openfiat_identity::IdentityRegistry;
use openfiat_network::Node;
use openfiat_notifications::NotificationRegistry;
use openfiat_oracles::OracleIndex;
use openfiat_registry::Registry as ServiceRegistry;
use openfiat_reputation::ReputationView;
use openfiat_reservations::ReservationRegistry;
use openfiat_rewards::{LivenessLedger, RewardParams};
use openfiat_risk::RiskIndex;
use openfiat_serialization::wire;
use openfiat_sessions::SessionRegistry;
use openfiat_settlement::SettlementRegistry;
use openfiat_snapshot::SnapshotIndex;
use openfiat_storage::KvStore;
use openfiat_trade::TradeView;
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
}

pub struct NodeState<S> {
    pub gossip: RefCell<GossipService<Rc<S>>>,
    pub advertisements: Rc<AdvertisementRegistry<Rc<S>>>,
    pub reservations: Rc<ReservationRegistry<Rc<S>>>,
    pub settlements: Rc<SettlementRegistry<Rc<S>>>,
    pub trades: TradeView<Rc<S>>,
    pub disputes: Rc<DisputeRegistry<Rc<S>>>,
    pub identity: Rc<IdentityRegistry<Rc<S>>>,
    pub reputation: ReputationView<Rc<S>>,
    pub governance: Rc<GovernanceRegistry<Rc<S>>>,
    pub services: Rc<ServiceRegistry<Rc<S>>>,
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
    ) -> Self {
        let store = Rc::new(store);
        let event_store = EventStore::new(Rc::clone(&store));
        let mut gossip =
            GossipService::new(node, event_store, keypair, self_roles, Subscription::All);

        let services = Rc::new(ServiceRegistry::new(Rc::clone(&store)));
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
        let reputation = ReputationView::new(
            Rc::clone(&reservations),
            Rc::clone(&settlements),
            Rc::clone(&disputes),
        );
        let identity = Rc::new(IdentityRegistry::new(Rc::clone(&store)));
        let governance = Rc::new(GovernanceRegistry::new(Rc::clone(&store)));
        let notifications = Rc::new(NotificationRegistry::new(
            Rc::clone(&store),
            Rc::clone(&services),
        ));
        let oracles = Rc::new(OracleIndex::new(Rc::clone(&store), Rc::clone(&services)));
        let risk = Rc::new(RiskIndex::new(Rc::clone(&store), Rc::clone(&services)));
        let snapshots = Rc::new(SnapshotIndex::new(Rc::clone(&store), Rc::clone(&services)));
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
                        });
                }
            } else {
                governance_for_events.apply_event(event);
            }
        });

        Self {
            gossip: RefCell::new(gossip),
            advertisements,
            reservations,
            settlements,
            trades,
            disputes,
            identity,
            reputation,
            governance,
            services,
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

    /// Queues a governance vote for independent on-chain stake
    /// verification — see `PendingVoteVerification`'s own doc for why
    /// its weight can't just be applied here directly.
    pub fn enqueue_vote_verification(&self, stake_account: String, signed_vote_bytes: Vec<u8>) {
        self.pending_vote_verifications
            .borrow_mut()
            .push_back(PendingVoteVerification {
                stake_account,
                signed_vote_bytes,
            });
    }

    /// Drains every vote currently queued for verification — called once
    /// per `actor::poll_vote_verifications` tick. Anything that fails
    /// verification this round (not yet observable, or a transient RPC
    /// error) is expected to be re-queued by the caller via
    /// `enqueue_vote_verification`, same retry shape as `ChainState`'s
    /// `awaiting_confirmation`.
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
    #[cfg(test)]
    pub fn new_for_test(store: S) -> Self {
        let keypair = Keypair::generate();
        let node = Node::new(&keypair).expect("loopback node construction cannot fail");
        Self::new(node, store, keypair, Vec::new(), NodeChainMode::GossipOnly)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfiat_storage::mem::MemoryStore;

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
