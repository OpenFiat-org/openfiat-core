//! Phase 5 exit criteria (OFS-1500): a service registers on one node and
//! replicates to every node in the cluster via gossip, a health-state
//! change propagates the same way, and a provider that stops publishing
//! health updates past the threshold auto-expires locally on every node.

use futures::future::select_all;
use openfiat_crypto::Keypair;
use openfiat_gossip::channel::Subscription;
use openfiat_gossip::EventStore;
use openfiat_network::identity::{peer_id, to_libp2p_keypair};
use openfiat_network::{Multiaddr, Node};
use openfiat_registry::health::HealthState;
use openfiat_registry::RegistryService;
use openfiat_storage::mem::MemoryStore;
use openfiat_types::{InfrastructureService, PeerId, PublicKey, ServiceType};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

fn make_service(seed: u8) -> RegistryService<MemoryStore> {
    let keypair = Keypair::from_seed([seed; 32]);
    let node = Node::new(&keypair).unwrap();
    let event_store = EventStore::new(MemoryStore::new());
    let gossip = openfiat_gossip::GossipService::new(node, event_store, keypair, vec![], Subscription::All);
    RegistryService::new(gossip, MemoryStore::new())
}

fn identity(seed: u8) -> (PeerId, PublicKey) {
    let keypair = Keypair::from_seed([seed; 32]);
    (peer_id(&to_libp2p_keypair(&keypair)), keypair.public_key())
}

async fn drive_until(services: &mut [RegistryService<MemoryStore>], mut condition: impl FnMut(&[RegistryService<MemoryStore>]) -> bool) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !condition(services) {
            let futures: Vec<Pin<Box<dyn Future<Output = ()> + '_>>> =
                services.iter_mut().map(|s| Box::pin(s.drive_once()) as Pin<Box<dyn Future<Output = ()> + '_>>).collect();
            select_all(futures).await;
        }
    })
    .await
    .expect("did not reach the expected state in time")
}

async fn listen_addr(service: &mut RegistryService<MemoryStore>) -> Multiaddr {
    service.gossip.node.listen_on("/ip4/127.0.0.1/udp/0/quic-v1".parse().unwrap()).unwrap();
    loop {
        if let libp2p::swarm::SwarmEvent::NewListenAddr { address, .. } = service.gossip.node.next_event().await {
            return address;
        }
    }
}

#[tokio::test]
async fn registration_and_health_changes_replicate_and_stale_services_expire_everywhere() {
    // Star: hub (index 0) directly connected to two leaves.
    let mut hub = make_service(1);
    let hub_addr = listen_addr(&mut hub).await;

    let mut leaves: Vec<RegistryService<MemoryStore>> = (2..=3).map(make_service).collect();
    for leaf in &mut leaves {
        leaf.gossip.node.dial(hub_addr.clone()).unwrap();
    }

    let mut all: Vec<RegistryService<MemoryStore>> = std::iter::once(hub).chain(leaves).collect();

    let seeds = [1u8, 2, 3];
    let identities: Vec<_> = seeds.iter().map(|&seed| identity(seed)).collect();
    for (i, service) in all.iter_mut().enumerate() {
        for (j, (peer_id, public_key)) in identities.iter().enumerate() {
            if i != j {
                service.gossip.register_peer_key(peer_id.clone(), *public_key);
            }
        }
    }

    drive_until(&mut all, |services| services.iter().all(|s| s.gossip.connected_peer_count() >= 1)).await;

    // Leaf 0 (index 1) registers a service.
    let service_id = all[1]
        .register(
            "snapshot-1",
            ServiceType::Infrastructure(InfrastructureService::SnapshotProvider),
            vec!["/ip4/127.0.0.1/udp/9001/quic-v1".to_string()],
            vec![1300],
            Some("Kenya".to_string()),
            vec!["hourly".to_string()],
            None,
        )
        .unwrap();

    drive_until(&mut all, |services| services.iter().all(|s| s.get(&service_id).is_some())).await;

    for service in &all {
        let record = service.get(&service_id).unwrap();
        assert_eq!(record.region.as_deref(), Some("Kenya"));
        assert_eq!(record.health, HealthState::Online);
    }

    // The same node publishes a health change; it must reach every node.
    all[1].publish_health(service_id.clone(), HealthState::Degraded).unwrap();
    drive_until(&mut all, |services| services.iter().all(|s| s.get(&service_id).unwrap().health == HealthState::Degraded)).await;

    // Auto-expiration (§18) is purely local bookkeeping — every node runs
    // it independently against its own replica. A zero-duration threshold
    // means "anything not updated just now is stale", which is true for
    // every node the instant after the health update above landed.
    for service in &all {
        let removed = service.expire_stale(Duration::from_millis(0));
        assert_eq!(removed, 1);
        assert!(service.get(&service_id).is_none());
    }
}
