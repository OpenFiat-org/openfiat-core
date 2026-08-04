//! A settlement's full happy path — initiate, submit payment, approve —
//! replicates correctly across a 2-node gossip cluster, with each action
//! signed and originated by the party who actually performs it.
//! `Approved` is the terminal state gossip alone reaches (OFS-4300):
//! completion is recorded once each node independently observes the
//! on-chain release confirmed, not via a further gossiped event — this
//! test proves that recording is available identically on both nodes,
//! not just the one that happened to originate the approval.

use futures::future::select_all;
use openfiat_advertisements::AdvertisementRegistry;
use openfiat_crypto::Keypair;
use openfiat_gossip::EventStore;
use openfiat_gossip::channel::Subscription;
use openfiat_network::identity::{peer_id, to_libp2p_keypair};
use openfiat_network::{Multiaddr, Node};
use openfiat_reservations::{ReservationId, ReservationRegistry};
use openfiat_settlement::{SettlementService, SettlementState};
use openfiat_storage::mem::MemoryStore;
use openfiat_types::{Amount, PeerId, PublicKey};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::time::Duration;

fn make_service(seed: u8) -> SettlementService<MemoryStore> {
    let keypair = Keypair::from_seed([seed; 32]);
    let node = Node::new(&keypair).unwrap();
    let event_store = EventStore::new(MemoryStore::new());
    let gossip =
        openfiat_gossip::GossipService::new(node, event_store, keypair, vec![], Subscription::All);
    SettlementService::new(
        gossip,
        MemoryStore::new(),
        Rc::new(ReservationRegistry::new(
            MemoryStore::new(),
            Rc::new(AdvertisementRegistry::new(MemoryStore::new())),
        )),
    )
}

fn identity(seed: u8) -> (PeerId, PublicKey) {
    let keypair = Keypair::from_seed([seed; 32]);
    (peer_id(&to_libp2p_keypair(&keypair)), keypair.public_key())
}

async fn drive_until(
    services: &mut [SettlementService<MemoryStore>],
    mut condition: impl FnMut(&[SettlementService<MemoryStore>]) -> bool,
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

async fn listen_addr(service: &mut SettlementService<MemoryStore>) -> Multiaddr {
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
async fn a_settlements_happy_path_replicates_across_the_cluster() {
    let mut buyer = make_service(1);
    let mut seller = make_service(2);

    let buyer_addr = listen_addr(&mut buyer).await;
    let (buyer_id, buyer_key) = identity(1);
    let (seller_id, seller_key) = identity(2);
    buyer
        .gossip
        .register_peer_key(seller_id.clone(), seller_key);
    seller.gossip.register_peer_key(buyer_id, buyer_key);
    seller.gossip.node.dial(buyer_addr).unwrap();

    let mut all = vec![buyer, seller];
    drive_until(&mut all, |services| {
        services
            .iter()
            .all(|s| s.gossip.connected_peer_count() >= 1)
    })
    .await;

    let settlement_id = all[0]
        .initiate(
            "settle-1",
            ReservationId::new("res-1"),
            seller_id,
            seller_key,
            Amount::new(2_000_000, 6),
        )
        .unwrap();
    drive_until(&mut all, |services| {
        services.iter().all(|s| s.get(&settlement_id).is_some())
    })
    .await;

    all[0]
        .submit_payment(settlement_id.clone(), Some("TXN123".to_string()))
        .unwrap();
    drive_until(&mut all, |services| {
        services
            .iter()
            .all(|s| s.get(&settlement_id).unwrap().state == SettlementState::PaymentSubmitted)
    })
    .await;
    for service in &all {
        assert_eq!(
            service
                .get(&settlement_id)
                .unwrap()
                .payment_reference
                .as_deref(),
            Some("TXN123")
        );
    }

    all[1].approve(settlement_id.clone()).unwrap();
    drive_until(&mut all, |services| {
        services
            .iter()
            .all(|s| s.get(&settlement_id).unwrap().state == SettlementState::Approved)
    })
    .await;

    // Completion is local bookkeeping (OFS-4300), not a further gossiped
    // event — each node records it independently once it observes the
    // on-chain release confirmed.
    for service in &mut all {
        service
            .record_escrow_released(&settlement_id, "5xY...onchainSig")
            .unwrap();
    }
    for service in &all {
        let settlement = service.get(&settlement_id).unwrap();
        assert_eq!(settlement.state, SettlementState::Completed);
        assert_eq!(
            settlement.escrow_release_signature.as_deref(),
            Some("5xY...onchainSig")
        );
    }
}
