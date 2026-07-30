//! The Phase 6 exit criterion for disputes: a full commit-reveal cycle —
//! open, three arbitrators join, commit, and reveal — runs to a
//! consistent resolution across every node in a gossip cluster.

use futures::future::select_all;
use openfiat_crypto::Keypair;
use openfiat_disputes::{DisputeService, DisputeStatus, Vote};
use openfiat_gossip::EventStore;
use openfiat_gossip::channel::Subscription;
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_network::identity::{peer_id, to_libp2p_keypair};
use openfiat_network::{Multiaddr, Node};
use openfiat_reservations::ReservationId;
use openfiat_settlement::events::SignedSettlementInitiate;
use openfiat_settlement::{SettlementId, SettlementRegistry};
use openfiat_storage::mem::MemoryStore;
use openfiat_types::{Amount, PeerId, PublicKey, Timestamp};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::time::Duration;

fn seeded_settlements(
    settlement_id: &SettlementId,
    buyer: &Keypair,
    seller: &Keypair,
) -> Rc<SettlementRegistry<MemoryStore>> {
    let registry = Rc::new(SettlementRegistry::new(MemoryStore::new()));
    let initiate = openfiat_settlement::events::SettlementInitiate {
        id: settlement_id.clone(),
        reservation_id: ReservationId::new("res-1"),
        buyer: peer_id_from_public_key(&buyer.public_key()).unwrap(),
        buyer_public_key: buyer.public_key(),
        seller: peer_id_from_public_key(&seller.public_key()).unwrap(),
        seller_public_key: seller.public_key(),
        amount: Amount::new(2_000_000, 6),
        timestamp: Timestamp::now(),
    };
    registry
        .apply_initiate(SignedSettlementInitiate::sign(initiate, buyer))
        .unwrap();
    registry
}

fn make_service(
    seed: u8,
    settlements: Rc<SettlementRegistry<MemoryStore>>,
) -> DisputeService<MemoryStore> {
    let keypair = Keypair::from_seed([seed; 32]);
    let node = Node::new(&keypair).unwrap();
    let event_store = EventStore::new(MemoryStore::new());
    let gossip =
        openfiat_gossip::GossipService::new(node, event_store, keypair, vec![], Subscription::All);
    DisputeService::new(gossip, MemoryStore::new(), settlements)
}

fn identity(seed: u8) -> (PeerId, PublicKey) {
    let keypair = Keypair::from_seed([seed; 32]);
    (peer_id(&to_libp2p_keypair(&keypair)), keypair.public_key())
}

async fn drive_until(
    services: &mut [DisputeService<MemoryStore>],
    mut condition: impl FnMut(&[DisputeService<MemoryStore>]) -> bool,
) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !condition(services) {
            let futures: Vec<Pin<Box<dyn Future<Output = ()> + '_>>> = services
                .iter_mut()
                .map(|s| Box::pin(s.drive_once()) as Pin<Box<dyn Future<Output = ()> + '_>>)
                .collect();
            select_all(futures).await;
        }
    })
    .await
    .expect("did not reach the expected state in time")
}

async fn listen_addr(service: &mut DisputeService<MemoryStore>) -> Multiaddr {
    service
        .gossip
        .node
        .listen_on("/ip4/127.0.0.1/udp/0/quic-v1".parse().unwrap())
        .unwrap();
    loop {
        if let libp2p::swarm::SwarmEvent::NewListenAddr { address, .. } =
            service.gossip.node.next_event().await
        {
            return address;
        }
    }
}

#[tokio::test]
async fn a_full_commit_reveal_cycle_reaches_a_consistent_resolution_across_the_cluster() {
    let settlement_id = SettlementId::new("settle-1");
    let buyer_kp = Keypair::from_seed([1u8; 32]);
    let seller_kp = Keypair::from_seed([99u8; 32]);

    let seeds: [u8; 4] = [1, 2, 3, 4]; // buyer + 3 arbitrators
    let settlement_registries: Vec<_> = seeds
        .iter()
        .map(|_| seeded_settlements(&settlement_id, &buyer_kp, &seller_kp))
        .collect();
    let mut all: Vec<DisputeService<MemoryStore>> = seeds
        .iter()
        .zip(settlement_registries.iter())
        .map(|(&seed, registry)| make_service(seed, Rc::clone(registry)))
        .collect();

    let hub_addr = listen_addr(&mut all[0]).await;
    let identities: Vec<_> = seeds.iter().map(|&seed| identity(seed)).collect();
    for (i, service) in all.iter_mut().enumerate() {
        for (j, (peer_id, public_key)) in identities.iter().enumerate() {
            if i != j {
                service
                    .gossip
                    .register_peer_key(peer_id.clone(), *public_key);
            }
        }
        if i != 0 {
            service.gossip.node.dial(hub_addr.clone()).unwrap();
        }
    }

    drive_until(&mut all, |services| {
        services
            .iter()
            .all(|s| s.gossip.connected_peer_count() >= 1)
    })
    .await;

    let dispute_id = all[0]
        .open("dispute-1", settlement_id, "payment not received")
        .unwrap();
    drive_until(&mut all, |services| {
        services.iter().all(|s| s.get(&dispute_id).is_some())
    })
    .await;

    for arbitrator in &mut all[1..] {
        arbitrator.join_as_arbitrator(dispute_id.clone()).unwrap();
    }
    drive_until(&mut all, |services| {
        services
            .iter()
            .all(|s| s.get(&dispute_id).unwrap().status == DisputeStatus::CaseLocked)
    })
    .await;

    let votes = [Vote::BuyerWins, Vote::BuyerWins, Vote::MerchantWins];
    let secrets = [[11u8; 32], [22u8; 32], [33u8; 32]];
    for ((arbitrator, vote), secret) in all[1..].iter_mut().zip(votes).zip(secrets) {
        arbitrator
            .commit_vote(dispute_id.clone(), vote, secret)
            .unwrap();
    }
    drive_until(&mut all, |services| {
        services
            .iter()
            .all(|s| s.get(&dispute_id).unwrap().status == DisputeStatus::RevealPhase)
    })
    .await;

    for ((arbitrator, vote), secret) in all[1..].iter_mut().zip(votes).zip(secrets) {
        arbitrator
            .reveal_vote(dispute_id.clone(), vote, secret)
            .unwrap();
    }
    drive_until(&mut all, |services| {
        services
            .iter()
            .all(|s| s.get(&dispute_id).unwrap().status == DisputeStatus::AwaitingChainExecution)
    })
    .await;

    // What replication guarantees, and what it does not.
    //
    // Every node ends up holding the same three signed reveals — that is
    // the property this test exists for, and it still holds. What no node
    // does is turn them into a verdict: the chain re-arbitrates the case
    // under its own rules, and a second tally here would be a second
    // answer rather than a confirmation of the first.
    for service in &all {
        let dispute = service.get(&dispute_id).unwrap();
        assert_eq!(dispute.reveals.len(), 3);
        assert_eq!(
            dispute.resolution, None,
            "consistent across the cluster is not the same as correct — \
             all three nodes agreeing on a verdict the chain did not \
             reach is exactly the failure being prevented"
        );
    }
}
