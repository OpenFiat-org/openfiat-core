//! Phase 3 exit criteria: a 4-5 node local cluster started from one
//! bootstrap node converges to a consistent peer set, entirely through
//! peer exchange (OFS-1100 §9) — no manual wiring beyond dialing the
//! bootstrap once.

use futures::future::select_all;
use openfiat_crypto::Keypair;
use openfiat_discovery::DiscoveryService;
use openfiat_discovery::cache::PeerCache;
use openfiat_network::Node;
use openfiat_storage::mem::MemoryStore;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

fn make_service(seed: u8) -> DiscoveryService<MemoryStore> {
    let keypair = Keypair::from_seed([seed; 32]);
    let node = Node::new(&keypair).unwrap();
    let cache = PeerCache::new(MemoryStore::new());
    DiscoveryService::new(
        node,
        cache,
        keypair.public_key(),
        "1.0.0",
        vec![1000, 1100],
        vec![],
        10,
    )
}

/// Drive every service concurrently until `converged` returns true.
async fn drive_until(
    services: &mut [DiscoveryService<MemoryStore>],
    mut converged: impl FnMut(&[DiscoveryService<MemoryStore>]) -> bool,
) {
    tokio::time::timeout(Duration::from_secs(30), async {
        while !converged(services) {
            let futures: Vec<Pin<Box<dyn Future<Output = ()> + '_>>> = services
                .iter_mut()
                .map(|s| Box::pin(s.drive_once()) as Pin<Box<dyn Future<Output = ()> + '_>>)
                .collect();
            select_all(futures).await;
        }
    })
    .await
    .expect("cluster did not converge in time")
}

#[tokio::test]
async fn a_five_node_cluster_converges_to_a_consistent_peer_set() {
    const NODE_COUNT: usize = 5;
    let mut services: Vec<DiscoveryService<MemoryStore>> =
        (1..=NODE_COUNT as u8).map(make_service).collect();

    // Every node listens (not just the bootstrap) — a node with no
    // dialable address of its own has nothing to advertise, so peer
    // exchange couldn't help anyone else discover it by address.
    for service in &mut services {
        service
            .node
            .listen_on("/ip4/127.0.0.1/udp/0/quic-v1".parse().unwrap())
            .unwrap();
    }
    drive_until(&mut services, |services| {
        services.iter().all(|s| !s.listen_addresses().is_empty())
    })
    .await;

    let bootstrap_addr: openfiat_network::Multiaddr =
        services[0].listen_addresses()[0].parse().unwrap();
    for service in &mut services[1..] {
        service.dial(bootstrap_addr.clone()).unwrap();
    }

    drive_until(&mut services, |services| {
        services.iter().all(|service| {
            let peers = service.cache.all().unwrap();
            peers.len() == NODE_COUNT - 1 && peers.iter().all(|record| !record.addresses.is_empty())
        })
    })
    .await;

    // Every node's peer set is exactly "everyone else" — no duplicates, no
    // missing peers, no leftover placeholders lacking a real address.
    let all_peer_ids: std::collections::HashSet<_> = services
        .iter()
        .map(|service| service.node.local_peer_id())
        .collect();
    assert_eq!(
        all_peer_ids.len(),
        NODE_COUNT,
        "every node must have a distinct peer ID"
    );

    for service in &services {
        let known: std::collections::HashSet<_> = service
            .cache
            .all()
            .unwrap()
            .into_iter()
            .map(|record| record.peer_id)
            .collect();
        let expected: std::collections::HashSet<_> = all_peer_ids
            .iter()
            .filter(|&peer_id| peer_id != &service.node.local_peer_id())
            .cloned()
            .collect();
        assert_eq!(known, expected);
    }
}
