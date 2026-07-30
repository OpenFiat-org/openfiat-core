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

/// A node paired with the discovery service that runs on it.
///
/// The service no longer owns a swarm — a node has one, and the actor
/// drives it (see `DiscoveryService`'s own doc for why owning a second one
/// is how this service ended up never running at all). So the pairing that
/// used to be a field is made here instead, which is also what the real
/// node does.
struct Peer {
    node: Node,
    service: DiscoveryService<MemoryStore>,
}

impl Peer {
    /// Wait for one swarm event and let discovery act on it.
    ///
    /// Routes the same way the node actor does: envelopes carrying
    /// discovery's own spec number go to `handle_message`, everything else
    /// is lifecycle. Written out rather than hidden behind a helper so a
    /// reader can see this test exercises the real routing rather than a
    /// convenience path that only exists for tests.
    async fn drive_once(&mut self) {
        let event = self.node.next_event().await;
        match event {
            libp2p::swarm::SwarmEvent::Behaviour(
                openfiat_network::behaviour::OpenFiatBehaviourEvent::Envelope(
                    libp2p::request_response::Event::Message { peer, message, .. },
                ),
            ) => self.service.handle_message(peer, message, &mut self.node),
            other => self.service.handle_lifecycle(&other, &mut self.node),
        }
    }
}

fn make_service(seed: u8) -> Peer {
    let keypair = Keypair::from_seed([seed; 32]);
    let node = Node::new(&keypair).unwrap();
    let cache = PeerCache::new(MemoryStore::new());
    let service = DiscoveryService::new(
        node.local_peer_id(),
        cache,
        keypair.public_key(),
        "1.0.0",
        vec![1000, 1100],
        vec![],
        10,
    );
    Peer { node, service }
}

/// Drive every service concurrently until `converged` returns true.
async fn drive_until(peers: &mut [Peer], mut converged: impl FnMut(&[Peer]) -> bool) {
    tokio::time::timeout(Duration::from_secs(30), async {
        while !converged(peers) {
            let futures: Vec<Pin<Box<dyn Future<Output = ()> + '_>>> = peers
                .iter_mut()
                .map(|p| Box::pin(p.drive_once()) as Pin<Box<dyn Future<Output = ()> + '_>>)
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
    let mut services: Vec<Peer> = (1..=NODE_COUNT as u8).map(make_service).collect();

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
        services
            .iter()
            .all(|s| !s.service.listen_addresses().is_empty())
    })
    .await;

    let bootstrap_addr: openfiat_network::Multiaddr =
        services[0].service.listen_addresses()[0].parse().unwrap();
    for service in &mut services[1..] {
        service.node.dial(bootstrap_addr.clone()).unwrap();
    }

    drive_until(&mut services, |services| {
        services.iter().all(|service| {
            let peers = service.service.cache.all().unwrap();
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
            .service
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
