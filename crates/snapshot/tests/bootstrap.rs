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
use openfiat_snapshot::events::SignedSnapshotAnnounce;
use openfiat_snapshot::location::SnapshotLocation;
use openfiat_snapshot::trust::TrustAnchors;
use openfiat_snapshot::{SnapshotError, SnapshotService, fetch, serve};
use openfiat_storage::mem::MemoryStore;
use openfiat_types::{
    InfrastructureService, MarketplaceService, PeerId, PublicKey, ServiceId, ServiceType, Timestamp,
};
use std::future::Future;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::rc::Rc;
use std::time::Duration;

/// A plausible devnet slot. Any value works — what matters is that the
/// producer supplies one it observed rather than inventing a counter.
const TEST_SLOT: u64 = 412_000_000;

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
    let anchor = bs58::encode(producer.public_key().as_bytes()).into_string();
    make_node_with(
        seed,
        producer,
        TrustAnchors::with_operator(&[anchor]).unwrap(),
    )
}

fn make_node_with(seed: u8, producer: &Keypair, anchors: TrustAnchors) -> TestNode {
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
    // The producer is added as a trust anchor, because these tests are
    // about the download-and-import pipeline rather than about who a
    // fresh node believes. Without it every import here fails as
    // `UntrustedFirstSnapshot` — which is the anchor gate working, and is
    // asserted directly in `a_fresh_node_refuses_a_snapshot_from_a_stranger`.
    let snapshots =
        SnapshotService::with_anchors(gossip, Rc::clone(&store), Rc::clone(&services), anchors);

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
/// returns the socket it bound. The port has to be known *before* the
/// snapshot is produced, since the announced location embeds it.
async fn serve_directory(directory: PathBuf) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, serve::router(directory))
            .await
            .unwrap();
    });
    address
}

/// The configuration of a node whose operator declared its public URL by
/// hand — the arrangement that used to be the only one, kept for the tests
/// that are about the transfer itself rather than where the URL came from.
fn configured(directory: &Path, address: SocketAddr) -> SnapshotConfig {
    SnapshotConfig {
        directory: directory.to_path_buf(),
        interval: Some(Duration::from_secs(60)),
        public_urls: vec![SnapshotLocation::parse(format!("http://{address}")).unwrap()],
        rpc_bind: None,
        retain: 3,
        trusted_providers: TrustAnchors::pinned(),
    }
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
    let address = serve_directory(directory.clone()).await;
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
    assert_eq!(nodes[1].snapshots.checkpoint_slot(), None);

    let config = configured(&directory, address);
    let producer_store = Rc::clone(&nodes[0].store);
    let metadata = nodes[0]
        .snapshots
        .produce_and_announce(
            &producer_store,
            SNAPSHOT_COLUMN_FAMILIES,
            &config,
            TEST_SLOT,
        )
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
        nodes[1].snapshots.checkpoint_slot(),
        Some(metadata.slot),
        "the joining node resumes from the snapshot height, not from full replay"
    );

    let _ = std::fs::remove_dir_all(&directory);
}

/// The `--snapshot-public-url` flag's replacement, end to end: nothing is
/// configured but the directory, and a joining node still downloads and
/// imports a real snapshot over real HTTP.
///
/// The producer's location comes from `SnapshotConfig::locations` applied
/// to an address of the kind a node learns about itself — a listen address
/// libp2p reported, or the `observed_addr` a peer sent back. `localhost`
/// stands in for the learned host because it is the only name that
/// resolves to the interface a test can actually bind: the derivation
/// itself does not care which host it is handed, as
/// `openfiat_snapshot::reachable`'s own tests cover, and what this test is
/// for is that the derived URL is one an HTTP client genuinely fetches
/// from.
#[tokio::test]
async fn a_node_given_no_public_url_still_announces_a_downloadable_snapshot() {
    let directory = temporary_directory("derived");
    let address = serve_directory(directory.clone()).await;
    let producer_keypair = Keypair::from_seed([5u8; 32]);

    let seeds: [u8; 2] = [5, 6];
    let mut nodes: Vec<TestNode> = seeds
        .iter()
        .map(|&seed| make_node(seed, &producer_keypair))
        .collect();
    seed_producer_only_state(&nodes[0]);
    connect(&mut nodes, &seeds).await;

    let config = SnapshotConfig {
        directory: directory.clone(),
        // The default bind. Unspecified rather than loopback, so the
        // server answers on every interface and any learned host is a
        // host a peer can ask.
        rpc_bind: Some(format!("0.0.0.0:{}", address.port()).parse().unwrap()),
        ..SnapshotConfig::default()
    };
    assert!(
        config.public_urls.is_empty(),
        "this test is void if anything was configured"
    );

    let learned: Vec<Multiaddr> = vec!["/dns4/localhost/udp/4001/quic-v1".parse().unwrap()];
    let base_urls = config.locations(&learned);
    assert_eq!(
        base_urls.iter().map(|l| l.to_string()).collect::<Vec<_>>(),
        [format!("http://localhost:{}", address.port())],
        "the node works out its own base URL from what it learned"
    );

    let (producer, producer_public_key) = nodes[0].snapshots.identity();
    let produced = openfiat_snapshot::producer::produce(
        &Rc::clone(&nodes[0].store),
        SNAPSHOT_COLUMN_FAMILIES,
        &config,
        &base_urls,
        7,
        producer,
        producer_public_key,
    )
    .expect("a derived location is a location");
    let metadata = produced.metadata.clone();
    nodes[0]
        .snapshots
        .announce_produced(produced.metadata)
        .expect("the producer is a registered snapshot provider");

    drive_until(&mut nodes, |nodes| nodes[1].snapshots.latest().is_some()).await;
    let discovered = nodes[1].snapshots.latest().unwrap();
    assert_eq!(discovered.locations, metadata.locations);

    let client = reqwest::Client::new();
    fetch::fetch_and_import(nodes[1].snapshots.index(), &client, &discovered.id)
        .await
        .expect("the derived URL must be one a peer can actually fetch from");
    assert!(
        nodes[1].services.get(&merchant_service_id()).is_some(),
        "the joining node holds state it never saw a gossip event for"
    );

    let _ = std::fs::remove_dir_all(&directory);
}

/// A producer announces every address it believes it is reachable at, and
/// on a multi-homed or NAT'd host some of them will not be — that is the
/// price of deriving them rather than being told. So the first location
/// that fails must cost a retry, not the snapshot.
#[tokio::test]
async fn a_dead_mirror_does_not_stop_the_next_location_from_working() {
    let directory = temporary_directory("dead-mirror");
    let live = serve_directory(directory.clone()).await;
    // A port nothing is listening on: bound to learn a free one, then
    // released. Refused outright, so the test does not wait on a timeout.
    let dead = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        listener.local_addr().unwrap()
    };
    let producer_keypair = Keypair::from_seed([7u8; 32]);

    let seeds: [u8; 2] = [7, 8];
    let mut nodes: Vec<TestNode> = seeds
        .iter()
        .map(|&seed| make_node(seed, &producer_keypair))
        .collect();
    seed_producer_only_state(&nodes[0]);
    connect(&mut nodes, &seeds).await;

    let config = SnapshotConfig {
        directory: directory.clone(),
        ..SnapshotConfig::default()
    };
    let (producer, producer_public_key) = nodes[0].snapshots.identity();
    let produced = openfiat_snapshot::producer::produce(
        &Rc::clone(&nodes[0].store),
        SNAPSHOT_COLUMN_FAMILIES,
        &config,
        &[
            SnapshotLocation::parse(format!("http://{dead}")).unwrap(),
            SnapshotLocation::parse(format!("http://{live}")).unwrap(),
        ],
        11,
        producer,
        producer_public_key,
    )
    .unwrap();
    let id = produced.metadata.id.clone();
    nodes[0]
        .snapshots
        .announce_produced(produced.metadata)
        .unwrap();
    drive_until(&mut nodes, |nodes| nodes[1].snapshots.latest().is_some()).await;

    let client = reqwest::Client::new();
    fetch::fetch_and_import(nodes[1].snapshots.index(), &client, &id)
        .await
        .expect("the second location must be tried after the first refuses");
    assert!(nodes[1].services.get(&merchant_service_id()).is_some());

    let _ = std::fs::remove_dir_all(&directory);
}

/// A mirror is untrusted by design (see `openfiat_snapshot::location`),
/// so the digest is the only thing standing between a joining node and a
/// forged worldview. One flipped byte on the wire must stop the import
/// dead and leave the store exactly as it was.
#[tokio::test]
async fn a_corrupted_download_is_rejected_and_changes_nothing() {
    let directory = temporary_directory("corrupted");
    let address = serve_directory(directory.clone()).await;
    let producer_keypair = Keypair::from_seed([3u8; 32]);

    let seeds: [u8; 2] = [3, 4];
    let mut nodes: Vec<TestNode> = seeds
        .iter()
        .map(|&seed| make_node(seed, &producer_keypair))
        .collect();
    seed_producer_only_state(&nodes[0]);
    connect(&mut nodes, &seeds).await;

    let config = configured(&directory, address);
    let producer_store = Rc::clone(&nodes[0].store);
    let metadata = nodes[0]
        .snapshots
        .produce_and_announce(
            &producer_store,
            SNAPSHOT_COLUMN_FAMILIES,
            &config,
            TEST_SLOT,
        )
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
        nodes[1].snapshots.checkpoint_slot(),
        None,
        "a rejected snapshot must not advance the checkpoint"
    );

    let _ = std::fs::remove_dir_all(&directory);
}

/// A node that trusts only the pinned anchors — i.e. not this test's
/// producer. Built by hand rather than through `make_node`, which anchors
/// its producer so the download tests can reach the pipeline.
fn make_untrusting_node(seed: u8, producer: &Keypair) -> TestNode {
    make_node_with(seed, producer, TrustAnchors::pinned())
}

/// The case the trust anchors exist for: a node with no history is offered
/// a snapshot that passes every other check, and refuses it.
///
/// The producer is registered, the announcement is signed, the bytes are
/// the announced size and they hash to the announced state root. That is
/// exactly the position an attacker occupies — those checks establish that
/// the bytes are what the announcer *said*, not that the announcer is
/// honest, and a node with no checkpoint has nothing to judge the claim
/// against. Its entire worldview would come from this file.
///
/// Note what this test deliberately does not use: `make_node` adds the
/// producer as a trust anchor, which is how every other test here gets to
/// exercise the download pipeline instead of this gate. The untrusting
/// node is built without that.
#[tokio::test]
async fn a_fresh_node_refuses_a_first_snapshot_from_a_stranger() {
    let directory = temporary_directory("untrusted-first-snapshot");
    let address = serve_directory(directory.clone()).await;
    let producer_keypair = Keypair::from_seed([1u8; 32]);

    let seeds: [u8; 2] = [1, 2];
    let mut nodes: Vec<TestNode> = seeds
        .iter()
        .map(|&seed| make_node(seed, &producer_keypair))
        .collect();
    seed_producer_only_state(&nodes[0]);
    connect(&mut nodes, &seeds).await;

    let config = configured(&directory, address);
    let producer_store = Rc::clone(&nodes[0].store);
    let metadata = nodes[0]
        .snapshots
        .produce_and_announce(
            &producer_store,
            SNAPSHOT_COLUMN_FAMILIES,
            &config,
            TEST_SLOT,
        )
        .expect("the producer is a registered snapshot provider");
    drive_until(&mut nodes, |nodes| nodes[1].snapshots.latest().is_some()).await;

    // The trusting node imports it, which establishes that everything
    // except trust is in order — so the refusal below is attributable to
    // the anchor gate and nothing else.
    let client = reqwest::Client::new();
    fetch::fetch_and_import(nodes[1].snapshots.index(), &client, &metadata.id)
        .await
        .expect("the pipeline itself is sound");

    // The same announcement, into a node that holds no checkpoint and does
    // not trust this producer.
    let untrusting = make_untrusting_node(3, &producer_keypair);
    assert_eq!(untrusting.snapshots.checkpoint_slot(), None);
    untrusting
        .snapshots
        .index()
        .apply_announce(SignedSnapshotAnnounce::sign(
            metadata.clone(),
            &producer_keypair,
        ))
        .expect("the announcement is valid; it is the import that is refused");

    let compressed =
        std::fs::read(directory.join(format!("{}.snapshot", metadata.id.as_str()))).unwrap();
    assert_eq!(
        untrusting
            .snapshots
            .index()
            .import(&metadata.id, &compressed),
        Err(SnapshotError::UntrustedFirstSnapshot),
        "a node with no history must not adopt a stranger's worldview, however well it verifies"
    );
    assert!(
        untrusting.services.get(&merchant_service_id()).is_none(),
        "nothing from the refused snapshot may reach the store"
    );
}
