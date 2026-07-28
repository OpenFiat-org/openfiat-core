//! The Phase 6c exit criterion for snapshot: a new node imports a
//! snapshot and resumes from the correct height, instead of full replay
//! — the announcement itself replicates across the cluster via gossip.

use futures::future::select_all;
use openfiat_crypto::Keypair;
use openfiat_gossip::EventStore;
use openfiat_gossip::channel::Subscription;
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_network::identity::{peer_id, to_libp2p_keypair};
use openfiat_network::{Multiaddr, Node};
use openfiat_registry::{Registration, Registry, SignedRegistration};
use openfiat_snapshot::SnapshotService;
use openfiat_storage::mem::MemoryStore;
use openfiat_types::{InfrastructureService, PeerId, PublicKey, ServiceId, ServiceType, Timestamp};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::time::Duration;

/// The same signed registration, independently applied to each node's
/// own local registry instance — mirroring `openfiat-disputes`'s
/// `seeded_settlements` helper: this crate's own replication test only
/// needs to exercise *its own* gossip channel, not `openfiat-registry`'s.
fn seeded_registry(producer: &Keypair, service_id: &str) -> Rc<Registry<MemoryStore>> {
    let registry = Rc::new(Registry::new(MemoryStore::new()));
    let registration = Registration {
        service_id: ServiceId::new(service_id),
        service_type: ServiceType::Infrastructure(InfrastructureService::SnapshotProvider),
        provider: peer_id_from_public_key(&producer.public_key()).unwrap(),
        provider_public_key: producer.public_key(),
        endpoints: vec!["https://snapshots.example/latest".to_string()],
        supported_ofs: vec![1300],
        region: None,
        capabilities: vec![],
        pricing: None,
        timestamp: Timestamp::now(),
    };
    registry
        .apply_registration(SignedRegistration::sign(registration, producer))
        .unwrap();
    registry
}

fn make_service(seed: u8, services: Rc<Registry<MemoryStore>>) -> SnapshotService<MemoryStore> {
    let keypair = Keypair::from_seed([seed; 32]);
    let node = Node::new(&keypair).unwrap();
    let event_store = EventStore::new(MemoryStore::new());
    let gossip =
        openfiat_gossip::GossipService::new(node, event_store, keypair, vec![], Subscription::All);
    SnapshotService::new(gossip, MemoryStore::new(), services)
}

fn identity(seed: u8) -> (PeerId, PublicKey) {
    let keypair = Keypair::from_seed([seed; 32]);
    (peer_id(&to_libp2p_keypair(&keypair)), keypair.public_key())
}

async fn drive_until(
    services: &mut [SnapshotService<MemoryStore>],
    mut condition: impl FnMut(&[SnapshotService<MemoryStore>]) -> bool,
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

async fn listen_addr(service: &mut SnapshotService<MemoryStore>) -> Multiaddr {
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
async fn a_new_node_discovers_imports_and_resumes_from_the_snapshot_height() {
    let seeds: [u8; 2] = [1, 2]; // producer + a joining new node
    let producer_keypair = Keypair::from_seed([1u8; 32]);
    let services = seeded_registry(&producer_keypair, "snap-svc-1");
    let mut all: Vec<SnapshotService<MemoryStore>> = seeds
        .iter()
        .map(|&seed| make_service(seed, Rc::clone(&services)))
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

    // The producer has already processed 4,217 events' worth of history
    // (OFS-1300 §9's own worked example) before generating this snapshot.
    let state_bytes = b"the complete marketplace state at height 4217";
    let (snapshot_id, compressed) = all[0].announce("snap-1", state_bytes, 4217).unwrap();

    // The announcement (metadata only, per §12) replicates to the new
    // node purely through gossip.
    drive_until(&mut all, |services| {
        services.iter().all(|s| s.latest().is_some())
    })
    .await;

    // The new node "downloads" the snapshot bytes out of band (§14 is
    // out of this crate's scope) and imports it.
    let joining_node = &all[1];
    let metadata = joining_node.latest().unwrap();
    assert_eq!(metadata.id, snapshot_id);
    assert_eq!(
        joining_node.checkpoint_height(),
        None,
        "hasn't imported anything yet"
    );

    let recovered_state = joining_node.import(&metadata, &compressed).unwrap();
    assert_eq!(recovered_state, state_bytes);

    // The new node now resumes from height 4217 instead of full replay.
    assert_eq!(joining_node.checkpoint_height(), Some(4217));
}
