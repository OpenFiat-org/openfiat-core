//! The Phase 6c exit criterion for snapshot, end to end and for real: a
//! producing node serializes its own state, writes and serves it over
//! HTTP, and a second node — which has never seen that state and cannot
//! answer a query about it — discovers the snapshot through gossip,
//! downloads it over the wire, verifies it, imports it, and afterwards
//! answers that query.
//!
//! Every hop is genuine: a real libp2p gossip mesh, a real axum server on
//! a real TCP port, a real `reqwest` download. Nothing here asserts that
//! a metadata field exists — the assertions are about state a node did
//! not have and then did.

use futures::future::select_all;
use openfiat_crypto::Keypair;
use openfiat_gossip::EventStore;
use openfiat_gossip::channel::Subscription;
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_network::identity::{peer_id, to_libp2p_keypair};
use openfiat_network::{Multiaddr, Node};
use openfiat_registry::{Registration, Registry, SignedRegistration};
use openfiat_snapshot::config::SnapshotConfig;
use openfiat_snapshot::location::SnapshotLocation;
use openfiat_snapshot::{SnapshotError, SnapshotService, fetch, serve};
use openfiat_storage::mem::MemoryStore;
use openfiat_types::{
    InfrastructureService, MarketplaceService, PeerId, PublicKey, ServiceId, ServiceType, Timestamp,
};
use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::rc::Rc;
use std::time::Duration;

/// The state a snapshot carries in this test. One column family is
/// enough to prove the mechanism, and the service registry is a good
/// choice: it is real domain state with a real query behind it
/// (`Registry::get`), and it is the same registry the import path checks
/// its authorization against.
const SNAPSHOT_COLUMN_FAMILIES: &[&str] = &["registry_services"];

type Store = Rc<MemoryStore>;

/// One node's world: a single physical store shared by its service
/// registry and its snapshot index, exactly as `NodeState` composes a
/// real node.
struct TestNode {
    store: Store,
    services: Rc<Registry<Store>>,
    snapshots: SnapshotService<Store>,
}

fn registration(keypair: &Keypair, service_id: &str, service_type: ServiceType) -> Registration {
    Registration {
        service_id: ServiceId::new(service_id),
        service_type,
        provider: peer_id_from_public_key(&keypair.public_key()).unwrap(),
        provider_public_key: keypair.public_key(),
        endpoints: vec![],
        supported_ofs: vec![1300],
        region: None,
        capabilities: vec![],
        pricing: None,
        payout_wallet: None,
        timestamp: Timestamp::now(),
    }
}

/// Every node starts knowing only that the producer is a registered
/// snapshot provider — without that, nothing it announces would be
/// importable. Each node holds its own replica of that fact, over its own
/// store, so the two stores start genuinely independent.
fn make_node(seed: u8, producer: &Keypair) -> TestNode {
    let store: Store = Rc::new(MemoryStore::new());
    let services = Rc::new(Registry::new(Rc::clone(&store)));
    services
        .apply_registration(SignedRegistration::sign(
            registration(
                producer,
                "snapshot-provider",
                ServiceType::Infrastructure(InfrastructureService::SnapshotProvider),
            ),
            producer,
        ))
        .unwrap();

    let keypair = Keypair::from_seed([seed; 32]);
    let node = Node::new(&keypair).unwrap();
    let event_store = EventStore::new(Rc::clone(&store));
    let gossip =
        openfiat_gossip::GossipService::new(node, event_store, keypair, vec![], Subscription::All);
    let snapshots = SnapshotService::new(gossip, Rc::clone(&store), Rc::clone(&services));

    TestNode {
        store,
        services,
        snapshots,
    }
}

fn identity(seed: u8) -> (PeerId, PublicKey) {
    let keypair = Keypair::from_seed([seed; 32]);
    (peer_id(&to_libp2p_keypair(&keypair)), keypair.public_key())
}

async fn drive_until(nodes: &mut [TestNode], mut condition: impl FnMut(&[TestNode]) -> bool) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !condition(nodes) {
            let futures: Vec<Pin<Box<dyn Future<Output = ()> + '_>>> = nodes
                .iter_mut()
                .map(|n| {
                    Box::pin(n.snapshots.drive_once()) as Pin<Box<dyn Future<Output = ()> + '_>>
                })
                .collect();
            select_all(futures).await;
        }
    })
    .await
    .expect("did not reach the expected state in time")
}

async fn listen_addr(node: &mut TestNode) -> Multiaddr {
    node.snapshots
        .gossip
        .node
        .listen_on("/ip4/127.0.0.1/udp/0/quic-v1".parse().unwrap())
        .unwrap();
    loop {
        if let libp2p::swarm::SwarmEvent::NewListenAddr { address, .. } =
            node.snapshots.gossip.node.next_event().await
        {
            return address;
        }
    }
}

async fn connect(nodes: &mut [TestNode], seeds: &[u8]) {
    let hub = listen_addr(&mut nodes[0]).await;
    let identities: Vec<_> = seeds.iter().map(|&seed| identity(seed)).collect();
    for (i, node) in nodes.iter_mut().enumerate() {
        for (j, (peer, public_key)) in identities.iter().enumerate() {
            if i != j {
                node.snapshots
                    .gossip
                    .register_peer_key(peer.clone(), *public_key);
            }
        }
        if i != 0 {
            node.snapshots.gossip.node.dial(hub.clone()).unwrap();
        }
    }
    drive_until(nodes, |nodes| {
        nodes
            .iter()
            .all(|n| n.snapshots.gossip.connected_peer_count() >= 1)
    })
    .await;
}

/// Starts the producer's archival endpoint on an ephemeral port and
/// returns the base URL to announce. The port has to be known *before*
/// the snapshot is produced, since the announced location embeds it —
/// which is exactly the ordering a real operator's configured public URL
/// removes the need to think about.
async fn serve_directory(directory: PathBuf) -> SnapshotLocation {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, serve::router(directory))
            .await
            .unwrap();
    });
    SnapshotLocation::parse(format!("http://{address}")).unwrap()
}

fn temporary_directory(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "openfiat-bootstrap-{name}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn merchant_service_id() -> ServiceId {
    ServiceId::new("merchant-only-the-producer-knows")
}

/// Registers a service with the producer alone. This is the state the
/// joining node has no way to learn except from a snapshot: it is
/// registered *before* the two nodes ever connect, so no gossip event
/// carrying it is ever emitted while the joining node is listening.
fn seed_producer_only_state(node: &TestNode) {
    let merchant = Keypair::from_seed([77u8; 32]);
    node.services
        .apply_registration(SignedRegistration::sign(
            {
                let mut registration = registration(
                    &merchant,
                    merchant_service_id().as_str(),
                    ServiceType::Marketplace(MarketplaceService::MerchantGateway),
                );
                registration.endpoints = vec!["https://merchant.example.com/pickup".to_string()];
                registration
            },
            &merchant,
        ))
        .unwrap();
}

#[tokio::test]
async fn a_joining_node_downloads_verifies_and_imports_a_real_snapshot() {
    let directory = temporary_directory("happy-path");
    let base_url = serve_directory(directory.clone()).await;
    let producer_keypair = Keypair::from_seed([1u8; 32]);

    let seeds: [u8; 2] = [1, 2]; // producer + a joining node
    let mut nodes: Vec<TestNode> = seeds
        .iter()
        .map(|&seed| make_node(seed, &producer_keypair))
        .collect();
    seed_producer_only_state(&nodes[0]);
    connect(&mut nodes, &seeds).await;

    // The query the joining node cannot answer yet. This is the whole
    // point of the test — everything below exists to change this answer.
    assert!(
        nodes[1].services.get(&merchant_service_id()).is_none(),
        "the joining node must start without the producer's state"
    );
    assert_eq!(nodes[1].snapshots.checkpoint_height(), None);

    let config = SnapshotConfig {
        directory: directory.clone(),
        interval: Some(Duration::from_secs(60)),
        public_urls: vec![base_url],
        retain: 3,
    };
    let producer_store = Rc::clone(&nodes[0].store);
    let metadata = nodes[0]
        .snapshots
        .produce_and_announce(&producer_store, SNAPSHOT_COLUMN_FAMILIES, &config)
        .expect("the producer is a registered snapshot provider");
    assert!(
        !metadata.locations.is_empty(),
        "an announcement without a location is the bug this closes"
    );

    // Only the metadata crosses the gossip mesh (§12).
    drive_until(&mut nodes, |nodes| nodes[1].snapshots.latest().is_some()).await;
    let discovered = nodes[1].snapshots.latest().unwrap();
    assert_eq!(discovered.id, metadata.id);
    assert_eq!(discovered.locations, metadata.locations);

    // The bytes cross real HTTP, from the announced URL.
    let client = reqwest::Client::new();
    let restored = fetch::fetch_and_import(nodes[1].snapshots.index(), &client, &discovered.id)
        .await
        .expect("a snapshot served by its own producer must verify");
    assert!(restored >= 2, "at least the two registered services");

    // The query the joining node could not answer before.
    let recovered = nodes[1]
        .services
        .get(&merchant_service_id())
        .expect("the imported snapshot must carry the producer's registry state");
    assert_eq!(
        recovered.endpoints,
        vec!["https://merchant.example.com/pickup".to_string()]
    );
    assert_eq!(
        nodes[1].snapshots.checkpoint_height(),
        Some(metadata.height),
        "the joining node resumes from the snapshot height, not from full replay"
    );

    let _ = std::fs::remove_dir_all(&directory);
}

/// A mirror is untrusted by design (see `openfiat_snapshot::location`),
/// so the digest is the only thing standing between a joining node and a
/// forged worldview. One flipped byte on the wire must stop the import
/// dead and leave the store exactly as it was.
#[tokio::test]
async fn a_corrupted_download_is_rejected_and_changes_nothing() {
    let directory = temporary_directory("corrupted");
    let base_url = serve_directory(directory.clone()).await;
    let producer_keypair = Keypair::from_seed([3u8; 32]);

    let seeds: [u8; 2] = [3, 4];
    let mut nodes: Vec<TestNode> = seeds
        .iter()
        .map(|&seed| make_node(seed, &producer_keypair))
        .collect();
    seed_producer_only_state(&nodes[0]);
    connect(&mut nodes, &seeds).await;

    let config = SnapshotConfig {
        directory: directory.clone(),
        interval: Some(Duration::from_secs(60)),
        public_urls: vec![base_url],
        retain: 3,
    };
    let producer_store = Rc::clone(&nodes[0].store);
    let metadata = nodes[0]
        .snapshots
        .produce_and_announce(&producer_store, SNAPSHOT_COLUMN_FAMILIES, &config)
        .unwrap();
    drive_until(&mut nodes, |nodes| nodes[1].snapshots.latest().is_some()).await;

    // Corrupt the served file in place, keeping its length identical so
    // the size check cannot catch this and the state root has to.
    let path = directory.join(format!("{}.snapshot", metadata.id.as_str()));
    let mut bytes = std::fs::read(&path).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0x01;
    std::fs::write(&path, &bytes).unwrap();

    let client = reqwest::Client::new();
    let result = fetch::fetch_and_import(nodes[1].snapshots.index(), &client, &metadata.id).await;
    assert_eq!(
        result,
        Err(SnapshotError::StateRootMismatch),
        "a single flipped byte must be fatal"
    );
    assert!(
        nodes[1].services.get(&merchant_service_id()).is_none(),
        "a rejected snapshot must not have written a single entry"
    );
    assert_eq!(
        nodes[1].snapshots.checkpoint_height(),
        None,
        "a rejected snapshot must not advance the checkpoint"
    );

    let _ = std::fs::remove_dir_all(&directory);
}
