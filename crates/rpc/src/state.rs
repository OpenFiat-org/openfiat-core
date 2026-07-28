//! `NodeState<S>` composes every domain registry this workspace has
//! built, all sharing one physical `S` (via `Rc<S>`'s `KvStore` impl —
//! see `openfiat-storage`) the same way a real node backs everything
//! with a single RocksDB `Database`. Constructed once, inside the
//! actor thread — see the `actor` module doc for why it can never cross
//! a thread boundary.

use openfiat_advertisements::AdvertisementRegistry;
use openfiat_chain::{ChainState, NodeChainMode};
use openfiat_disputes::DisputeRegistry;
use openfiat_governance::GovernanceRegistry;
use openfiat_identity::IdentityRegistry;
use openfiat_notifications::NotificationRegistry;
use openfiat_oracles::OracleIndex;
use openfiat_registry::Registry as ServiceRegistry;
use openfiat_reputation::ReputationView;
use openfiat_reservations::ReservationRegistry;
use openfiat_risk::RiskIndex;
use openfiat_sessions::SessionRegistry;
use openfiat_settlement::SettlementRegistry;
use openfiat_snapshot::SnapshotIndex;
use openfiat_storage::KvStore;
use openfiat_trade::TradeView;
use std::rc::Rc;

pub struct NodeState<S> {
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
}

impl<S: KvStore + 'static> NodeState<S> {
    pub fn new(store: S) -> Self {
        let store = Rc::new(store);
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
        // `GossipOnly` is the safe, zero-config default — an operator who
        // wants `RpcConnected` mode configures it explicitly at the
        // node-composition layer (`openfiat-cli`), same as every other
        // deployment-specific choice.
        let chain = Rc::new(ChainState::new(NodeChainMode::GossipOnly));

        Self {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfiat_storage::mem::MemoryStore;

    #[test]
    fn composes_without_panicking_and_starts_empty() {
        let state = NodeState::new(MemoryStore::new());
        assert!(state.advertisements.all().is_empty());
        assert!(state.trades.all().is_empty());
        assert!(state.services.all().is_empty());
    }
}
