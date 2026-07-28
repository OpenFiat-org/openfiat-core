//! Proves `ChainGossipService` end to end over a real (in-process) gossip
//! cluster: blockhash announcement propagates and updates every node's
//! `current_blockhash()`, a duplicate (blockhash, slot) from a different
//! origin does not get relayed onward, and a transaction relay request
//! fires a registered handler on every node that stores it.

use futures::future::select_all;
use openfiat_chain::ChainGossipService;
use openfiat_crypto::Keypair;
use openfiat_gossip::channel::Subscription;
use openfiat_gossip::{EventStore, GossipService};
use openfiat_network::identity::{peer_id, to_libp2p_keypair};
use openfiat_network::{Multiaddr, Node};
use openfiat_storage::mem::MemoryStore;
use openfiat_types::{PeerId, PublicKey};
use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::time::Duration;

fn make_service(seed: u8) -> ChainGossipService<MemoryStore> {
    let keypair = Keypair::from_seed([seed; 32]);
    let node = Node::new(&keypair).unwrap();
    let store = EventStore::new(MemoryStore::new());
    let gossip = GossipService::new(node, store, keypair, vec![], Subscription::All);
    ChainGossipService::new(gossip)
}

fn identity(seed: u8) -> (PeerId, PublicKey) {
    let keypair = Keypair::from_seed([seed; 32]);
    (peer_id(&to_libp2p_keypair(&keypair)), keypair.public_key())
}

async fn drive_until(
    services: &mut [ChainGossipService<MemoryStore>],
    mut condition: impl FnMut(&[ChainGossipService<MemoryStore>]) -> bool,
) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !condition(services) {
            let futures: Vec<Pin<Box<dyn Future<Output = ()> + '_>>> = services
                .iter_mut()
                .map(|s| Box::pin(s.gossip.drive_once()) as Pin<Box<dyn Future<Output = ()> + '_>>)
                .collect();
            select_all(futures).await;
        }
    })
    .await
    .expect("did not reach the expected state in time")
}

async fn drive_briefly(services: &mut [ChainGossipService<MemoryStore>], window: Duration) {
    let _ = tokio::time::timeout(window, async {
        loop {
            let futures: Vec<Pin<Box<dyn Future<Output = ()> + '_>>> = services
                .iter_mut()
                .map(|s| Box::pin(s.gossip.drive_once()) as Pin<Box<dyn Future<Output = ()> + '_>>)
                .collect();
            select_all(futures).await;
        }
    })
    .await;
}

async fn listen_addr(service: &mut ChainGossipService<MemoryStore>) -> Multiaddr {
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
async fn a_blockhash_announcement_propagates_and_updates_every_nodes_current_view() {
    let mut a = make_service(40);
    let mut b = make_service(41);

    let a_addr = listen_addr(&mut a).await;
    let (a_id, a_key) = identity(40);
    let (b_id, b_key) = identity(41);
    a.gossip.register_peer_key(b_id, b_key);
    b.gossip.register_peer_key(a_id, a_key);
    b.gossip.node.dial(a_addr).unwrap();

    let mut all = vec![a, b];
    drive_until(&mut all, |services| {
        services
            .iter()
            .all(|s| s.gossip.connected_peer_count() >= 1)
    })
    .await;

    assert_eq!(all[0].current_blockhash(), None, "nothing announced yet");
    all[0].announce_blockhash("hash-abc", 100).unwrap();

    drive_until(&mut all, |services| {
        services[1].current_blockhash().is_some()
    })
    .await;

    assert_eq!(
        all[1].current_blockhash(),
        Some(("hash-abc".to_string(), 100))
    );
}

#[tokio::test]
async fn two_origins_announcing_the_same_content_only_relay_once_past_the_hub() {
    // Star: w and x both connect to hub y; z connects to y too. w and x
    // each independently announce the identical (blockhash, slot) — two
    // distinct events (different origin/signature/id) with the same
    // content. y must store both (they're genuinely different events)
    // but relay only the first one on to z — content-addressed dedup,
    // not `EventId`-based dedup, is what has to catch the second.
    let mut w = make_service(80);
    let mut x = make_service(81);
    let mut y = make_service(82);
    let mut z = make_service(83);

    let y_addr = listen_addr(&mut y).await;
    let (w_id, w_key) = identity(80);
    let (x_id, x_key) = identity(81);
    let (y_id, y_key) = identity(82);
    let (z_id, z_key) = identity(83);

    // y must verify whoever originates; z must verify a relayed event's
    // *original* origin (w or x), not just y's.
    y.gossip.register_peer_key(w_id.clone(), w_key);
    y.gossip.register_peer_key(x_id.clone(), x_key);
    y.gossip.register_peer_key(z_id, z_key);
    z.gossip.register_peer_key(w_id, w_key);
    z.gossip.register_peer_key(x_id, x_key);
    z.gossip.register_peer_key(y_id, y_key);

    w.gossip.node.dial(y_addr.clone()).unwrap();
    x.gossip.node.dial(y_addr.clone()).unwrap();
    z.gossip.node.dial(y_addr).unwrap();

    let mut all = vec![w, x, y, z];
    drive_until(&mut all, |services| {
        services[2].gossip.connected_peer_count() >= 3
    })
    .await;

    all[0].announce_blockhash("hash-shared", 300).unwrap();
    drive_until(&mut all, |services| services[3].gossip.event_count() >= 1).await;

    all[1].announce_blockhash("hash-shared", 300).unwrap();
    drive_until(&mut all, |services| services[2].gossip.event_count() >= 2).await;

    drive_briefly(&mut all, Duration::from_millis(500)).await;

    assert_eq!(
        all[2].gossip.event_count(),
        2,
        "the hub stores both distinct events"
    );
    assert_eq!(
        all[3].gossip.event_count(),
        1,
        "the hub must relay only the first announcement of this content onward, not both"
    );
}

#[tokio::test]
async fn a_transaction_relay_request_fires_the_registered_handler_on_the_receiving_node() {
    let mut a = make_service(60);
    let mut b = make_service(61);

    let a_addr = listen_addr(&mut a).await;
    let (a_id, a_key) = identity(60);
    let (b_id, b_key) = identity(61);
    a.gossip.register_peer_key(b_id, b_key);
    b.gossip.register_peer_key(a_id, a_key);
    b.gossip.node.dial(a_addr).unwrap();

    let seen: Rc<RefCell<Vec<Vec<u8>>>> = Rc::new(RefCell::new(Vec::new()));
    let handler_seen = Rc::clone(&seen);
    b.on_relay_requested(move |request| handler_seen.borrow_mut().push(request.tx_bytes.clone()));

    let mut all = vec![a, b];
    drive_until(&mut all, |services| {
        services
            .iter()
            .all(|s| s.gossip.connected_peer_count() >= 1)
    })
    .await;

    all[0]
        .request_transaction_relay(b"a-signed-solana-transaction".to_vec())
        .unwrap();
    drive_until(&mut all, |_| !seen.borrow().is_empty()).await;

    assert_eq!(seen.borrow()[0], b"a-signed-solana-transaction");
}
