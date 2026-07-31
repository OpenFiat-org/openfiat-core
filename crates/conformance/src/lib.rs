//! A single in-process "full node": every domain registry this workspace
//! has, composed over one real, gossip-connected [`GossipService`] — the
//! same shared-store composition `openfiat-rpc`'s `NodeState` uses (see
//! that crate's `state` module), but actually wired to gossip, so a
//! mutation on one node in a cluster genuinely propagates to the rest
//! instead of staying local.
//!
//! `openfiat-rpc` deliberately skips this wiring (its `sendX` handlers
//! apply straight to a local, unconnected registry) because a JSON-RPC
//! node's own exit criteria never needed cross-node propagation. This
//! crate exists because Phase 10 does: a realistic multi-node harness for
//! integration and conformance testing, not a fourteenth copy of
//! `NodeState`. It is not part of the shipped node — see the crate
//! README.

use openfiat_advertisements::AdvertisementRegistry;
use openfiat_chain::{ChainBridge, ChainError};
use openfiat_crypto::Keypair;
use openfiat_disputes::DisputeRegistry;
use openfiat_gossip::{EventStore, GossipError, GossipService, Subscription};
use openfiat_governance::GovernanceRegistry;
use openfiat_identity::IdentityRegistry;
use openfiat_network::identity::{peer_id, to_libp2p_keypair};
use openfiat_network::{Multiaddr, Node};
use openfiat_notifications::NotificationRegistry;
use openfiat_oracles::OracleIndex;
use openfiat_registry::Registry as ServiceRegistry;
use openfiat_reputation::ReputationView;
use openfiat_reservations::ReservationRegistry;
use openfiat_risk::RiskIndex;
use openfiat_serialization::wire;
use openfiat_sessions::SessionRegistry;
use openfiat_settlement::SettlementRegistry;
use openfiat_snapshot::SnapshotIndex;
use openfiat_storage::KvStore;
use openfiat_trade::TradeView;
use openfiat_types::{EventType, NodeRole, PeerId, Priority, PublicKey};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

pub struct FullNode<S> {
    pub gossip: GossipService<Rc<S>>,
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
    pub chain: ChainBridge,
}

impl<S: KvStore + 'static> FullNode<S> {
    pub fn new(node: Node, store: S, keypair: Keypair, self_roles: Vec<NodeRole>) -> Self {
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
        // This harness never imports a snapshot — it exercises the
        // gossip surface — so there is genuinely nothing here to check.
        // A real node passes `openfiat_rpc::state::verify_snapshot_entry`.
        let snapshots = Rc::new(SnapshotIndex::new(
            Rc::clone(&store),
            Rc::clone(&services),
            openfiat_snapshot::state::accept_any,
        ));
        let sessions = Rc::new(SessionRegistry::new(Rc::clone(&store)));
        let chain = ChainBridge::install(&mut gossip);

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
        attach!(identity);
        attach!(governance);
        attach!(services);
        attach!(notifications);
        attach!(oracles);
        attach!(risk);
        attach!(snapshots);
        attach!(sessions);

        Self {
            gossip,
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
        }
    }

    /// OFS-4300 §6 — see [`ChainBridge::announce_blockhash`].
    pub fn announce_blockhash(&mut self, blockhash: &str, slot: u64) -> Result<(), ChainError> {
        self.chain
            .announce_blockhash(&mut self.gossip, blockhash, slot)
            .map(|_| ())
    }

    /// This node's current blockhash view — see
    /// [`ChainBridge::current_blockhash`].
    pub fn current_blockhash(&self) -> Option<(String, u64)> {
        self.chain.current_blockhash()
    }

    /// OFS-4300 §7 — see [`ChainBridge::request_transaction_relay`].
    pub fn request_transaction_relay(
        &mut self,
        tx_bytes: Vec<u8>,
        correlation: Option<String>,
    ) -> Result<(), ChainError> {
        self.chain
            .request_transaction_relay(&mut self.gossip, tx_bytes, correlation)
            .map(|_| ())
    }

    /// OFS-4300 §7 — see [`ChainBridge::announce_relay_confirmation`].
    pub fn announce_relay_confirmation(
        &mut self,
        signature: &str,
        slot_submitted: u64,
    ) -> Result<(), ChainError> {
        self.chain
            .announce_relay_confirmation(&mut self.gossip, signature, slot_submitted)
            .map(|_| ())
    }

    /// Sign, wrap, and broadcast a payload as a new gossip event — the
    /// same three steps every domain crate's own `*Service::originate`
    /// performs, exposed generically since this harness composes many
    /// domains on one node rather than being one domain's service.
    pub fn originate(
        &mut self,
        event_type: &str,
        ofs_spec: u16,
        priority: Priority,
        ttl: u8,
        payload: &impl serde::Serialize,
    ) -> Result<(), GossipError> {
        let bytes = wire::to_bytes(payload).expect("conformance harness payloads always serialize");
        let event_type =
            EventType::new(event_type).expect("event type is a valid PascalCase identifier");
        self.gossip
            .originate(event_type, ofs_spec, priority, ttl, bytes)
            .map(|_| ())
    }

    pub async fn drive_once(&mut self) {
        self.gossip.drive_once().await;
    }
}

/// This node's libp2p-derived identity, for cluster bootstrap (peer key
/// registration, dialing) — the same helper every existing multi-node
/// test in this workspace uses.
pub fn identity(keypair: &Keypair) -> (PeerId, PublicKey) {
    (peer_id(&to_libp2p_keypair(keypair)), keypair.public_key())
}

/// Bring up `n` nodes, each listening on its own QUIC/loopback address,
/// dialed into a single hub (node 0) and mutually peer-key-registered, so
/// every node can validate every other node's originated events. Mirrors
/// the bootstrap sequence `governance/tests/replication.rs` and friends
/// already hand-roll per test file — centralized here so whole-stack
/// tests don't repeat it.
pub async fn spawn_cluster<S: KvStore + 'static>(
    build_store: impl Fn() -> S,
    roles: &[Vec<NodeRole>],
) -> Vec<FullNode<S>> {
    let seeds: Vec<u8> = (0..roles.len()).map(|i| (i + 1) as u8).collect();
    let identities: Vec<(PeerId, PublicKey)> = seeds
        .iter()
        .map(|&seed| identity(&Keypair::from_seed([seed; 32])))
        .collect();

    let mut nodes: Vec<FullNode<S>> = seeds
        .iter()
        .zip(roles.iter())
        .map(|(&seed, roles)| {
            let node = Node::new(&Keypair::from_seed([seed; 32])).unwrap();
            FullNode::new(
                node,
                build_store(),
                Keypair::from_seed([seed; 32]),
                roles.clone(),
            )
        })
        .collect();

    let hub_addr = listen_addr(&mut nodes[0]).await;
    for (i, node) in nodes.iter_mut().enumerate() {
        for (j, (peer_id, public_key)) in identities.iter().enumerate() {
            if i != j {
                node.gossip.register_peer_key(peer_id.clone(), *public_key);
            }
        }
        if i != 0 {
            node.gossip.node.dial(hub_addr.clone()).unwrap();
        }
    }

    drive_until(&mut nodes, |nodes| {
        nodes.iter().all(|n| n.gossip.connected_peer_count() >= 1)
    })
    .await;

    nodes
}

async fn listen_addr<S: KvStore + 'static>(node: &mut FullNode<S>) -> Multiaddr {
    node.gossip
        .node
        .listen_on("/ip4/127.0.0.1/udp/0/quic-v1".parse().unwrap())
        .unwrap();
    loop {
        if let libp2p::swarm::SwarmEvent::NewListenAddr { address, .. } =
            node.gossip.node.next_event().await
        {
            return address;
        }
    }
}

/// Drive every node's gossip loop until `condition` holds, or panic after
/// 15 seconds — the same bounded-wait pattern every replication test in
/// this workspace already uses, generalized over the composed
/// [`FullNode`] instead of one domain's `*Service`.
pub async fn drive_until<S: KvStore + 'static>(
    nodes: &mut [FullNode<S>],
    mut condition: impl FnMut(&[FullNode<S>]) -> bool,
) {
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while !condition(nodes) {
            let futures: Vec<Pin<Box<dyn Future<Output = ()> + '_>>> = nodes
                .iter_mut()
                .map(|n| Box::pin(n.drive_once()) as Pin<Box<dyn Future<Output = ()> + '_>>)
                .collect();
            futures::future::select_all(futures).await;
        }
    })
    .await
    .expect("did not reach the expected state within 15 seconds")
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfiat_storage::mem::MemoryStore;

    #[tokio::test]
    async fn a_cluster_of_three_converges_to_fully_connected() {
        let roles = vec![vec![], vec![], vec![]];
        let nodes = spawn_cluster(MemoryStore::new, &roles).await;
        assert_eq!(nodes.len(), 3);
        assert!(nodes.iter().all(|n| n.advertisements.all().is_empty()));
    }
}
