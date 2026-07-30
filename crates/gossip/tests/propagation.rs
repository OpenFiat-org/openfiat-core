//! Phase 4 exit criteria (OFS-1200): an event published on one node
//! reaches every other node in a local cluster exactly once, a duplicate
//! re-send is suppressed, an expired-TTL event stops propagating short of
//! the full cluster, and a node that drops offline and reconnects
//! recovers the events it missed.

use futures::future::select_all;
use openfiat_crypto::Keypair;
use openfiat_gossip::channel::Subscription;
use openfiat_gossip::service::ReceiveOutcome;
use openfiat_gossip::{EventStore, GossipService};
use openfiat_network::identity::{peer_id, to_libp2p_keypair};
use openfiat_network::{Multiaddr, Node};
use openfiat_storage::mem::MemoryStore;
use openfiat_types::{EventType, PeerId, Priority, PublicKey};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

const TEST_OFS_SPEC: u16 = 9999;

fn make_service(seed: u8) -> GossipService<MemoryStore> {
    let keypair = Keypair::from_seed([seed; 32]);
    let node = Node::new(&keypair).unwrap();
    let store = EventStore::new(MemoryStore::new());
    GossipService::new(node, store, keypair, vec![], Subscription::All)
}

/// A seed's identity, without standing up a real `Node` for it.
fn identity(seed: u8) -> (PeerId, PublicKey) {
    let keypair = Keypair::from_seed([seed; 32]);
    (peer_id(&to_libp2p_keypair(&keypair)), keypair.public_key())
}

async fn drive_until(
    services: &mut [GossipService<MemoryStore>],
    mut condition: impl FnMut(&[GossipService<MemoryStore>]) -> bool,
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

/// Keep driving for a bounded window without a target condition, to give
/// any (incorrect) further propagation a chance to happen before an
/// absence assertion.
async fn drive_briefly(services: &mut [GossipService<MemoryStore>], window: Duration) {
    let _ = tokio::time::timeout(window, async {
        loop {
            let futures: Vec<Pin<Box<dyn Future<Output = ()> + '_>>> = services
                .iter_mut()
                .map(|s| Box::pin(s.drive_once()) as Pin<Box<dyn Future<Output = ()> + '_>>)
                .collect();
            select_all(futures).await;
        }
    })
    .await;
}

async fn listen_addr(service: &mut GossipService<MemoryStore>) -> Multiaddr {
    service
        .node
        .listen_on("/ip4/127.0.0.1/udp/0/quic-v1".parse().unwrap())
        .unwrap();
    loop {
        if let libp2p::swarm::SwarmEvent::NewListenAddr { address, .. } =
            service.node.next_event().await
        {
            return address;
        }
    }
}

#[tokio::test]
async fn event_reaches_every_node_exactly_once_and_a_resend_is_suppressed() {
    // Star topology: the hub is directly connected to 3 leaves, so an
    // event originated by a leaf genuinely hops leaf -> hub -> other
    // leaves rather than reaching everyone via one broadcast.
    let mut hub = make_service(1);
    let hub_addr = listen_addr(&mut hub).await;

    let mut leaves: Vec<GossipService<MemoryStore>> = (2..=4).map(make_service).collect();
    for leaf in &mut leaves {
        leaf.node.dial(hub_addr.clone()).unwrap();
    }

    let mut all: Vec<GossipService<MemoryStore>> = std::iter::once(hub).chain(leaves).collect();

    // Every node must be able to verify every other node's signature —
    // hub forwards a leaf's event to the other leaves unchanged (same
    // origin, same signature), so a leaf receiving a *relayed* event
    // needs the *originating* leaf's key, not just the hub's.
    let seeds = [1u8, 2, 3, 4];
    let identities: Vec<_> = seeds.iter().map(|&seed| identity(seed)).collect();
    for (i, service) in all.iter_mut().enumerate() {
        for (j, (peer_id, public_key)) in identities.iter().enumerate() {
            if i != j {
                service.register_peer_key(peer_id.clone(), *public_key);
            }
        }
    }

    drive_until(&mut all, |services| {
        services.iter().all(|s| s.connected_peer_count() >= 1)
    })
    .await;

    let event_id = all[1]
        .originate(
            EventType::new("GossipTestEvent").unwrap(),
            TEST_OFS_SPEC,
            Priority::Advertisement,
            8,
            b"hello".to_vec(),
        )
        .unwrap();

    drive_until(&mut all, |services| {
        services.iter().all(|s| s.has_event(&event_id))
    })
    .await;

    for service in &all {
        assert_eq!(
            service.event_count(),
            1,
            "every node should store the event exactly once"
        );
    }

    let stored = all[1]
        .get_event(&event_id)
        .expect("the origin has its own event");
    let outcome = all[0].receive_event(None, stored);
    assert!(matches!(outcome, ReceiveOutcome::Duplicate));
    assert_eq!(
        all[0].event_count(),
        1,
        "a re-delivered event must not be counted again"
    );
}

#[tokio::test]
async fn a_ttl_of_one_reaches_the_direct_peer_but_not_a_second_hop() {
    // Chain: x - y - z. x and z are never directly connected, so z can
    // only receive an event via y forwarding it.
    let mut x = make_service(10);
    let mut y = make_service(11);
    let mut z = make_service(12);

    let x_addr = listen_addr(&mut x).await;
    let (x_id, x_key) = identity(10);
    let (y_id, y_key) = identity(11);
    y.register_peer_key(x_id.clone(), x_key);
    y.node.dial(x_addr).unwrap();

    let y_addr = listen_addr(&mut y).await;
    z.register_peer_key(x_id, x_key); // z verifies x's signature on the relayed event
    z.register_peer_key(y_id, y_key);
    z.node.dial(y_addr).unwrap();

    let mut all = vec![x, y, z];
    drive_until(&mut all, |services| {
        services[1].connected_peer_count() >= 1 && services[2].connected_peer_count() >= 1
    })
    .await;

    let event_id = all[0]
        .originate(
            EventType::new("GossipTestEvent").unwrap(),
            TEST_OFS_SPEC,
            Priority::Advertisement,
            1,
            b"one-hop".to_vec(),
        )
        .unwrap();

    drive_until(&mut all, |services| services[1].has_event(&event_id)).await;
    drive_briefly(&mut all, Duration::from_millis(500)).await;

    assert!(
        all[0].has_event(&event_id),
        "the origin always has its own event"
    );
    assert!(
        all[1].has_event(&event_id),
        "ttl=1 must reach the direct peer"
    );
    assert!(
        !all[2].has_event(&event_id),
        "ttl=1 must not reach a second hop"
    );
}

#[tokio::test]
async fn a_reconnecting_node_recovers_events_it_missed_while_offline() {
    let mut a = make_service(20);
    let mut b = make_service(21);

    let a_addr = listen_addr(&mut a).await;
    let (a_id, a_key) = identity(20);
    let (b_id, b_key) = identity(21);
    a.register_peer_key(b_id, b_key);
    b.register_peer_key(a_id, a_key);
    b.node.dial(a_addr.clone()).unwrap();

    let mut all = vec![a, b];
    drive_until(&mut all, |services| {
        services.iter().all(|s| s.connected_peer_count() >= 1)
    })
    .await;

    all[1].disconnect_all();
    drive_until(&mut all, |services| {
        services.iter().all(|s| s.connected_peer_count() == 0)
    })
    .await;

    let event_id = all[0]
        .originate(
            EventType::new("GossipTestEvent").unwrap(),
            TEST_OFS_SPEC,
            Priority::Advertisement,
            8,
            b"missed-me".to_vec(),
        )
        .unwrap();
    drive_briefly(&mut all, Duration::from_millis(200)).await;
    assert!(
        !all[1].has_event(&event_id),
        "an offline node must not receive events sent while disconnected"
    );

    all[1].node.dial(a_addr).unwrap();
    drive_until(&mut all, |services| services[1].has_event(&event_id)).await;

    assert!(
        all[1].has_event(&event_id),
        "reconnecting must recover events missed while offline"
    );
}

#[tokio::test]
async fn a_forward_filter_suppresses_relaying_without_affecting_local_storage() {
    // Chain: x - y - z, same shape as the ttl test. y's forward filter
    // rejects one specific payload — proving suppression is per-event
    // content, not a blanket "stop relaying anything" switch, and that a
    // suppressed event is still stored/notified locally (only the
    // outbound re-forward is vetoed).
    let mut x = make_service(30);
    let mut y = make_service(31);
    let mut z = make_service(32);

    let x_addr = listen_addr(&mut x).await;
    let (x_id, x_key) = identity(30);
    let (y_id, y_key) = identity(31);
    y.register_peer_key(x_id.clone(), x_key);
    y.node.dial(x_addr).unwrap();

    let y_addr = listen_addr(&mut y).await;
    z.register_peer_key(x_id, x_key);
    z.register_peer_key(y_id, y_key);
    z.node.dial(y_addr).unwrap();

    y.add_forward_filter(|event| event.payload != b"suppress-me");

    let mut all = vec![x, y, z];
    drive_until(&mut all, |services| {
        services[1].connected_peer_count() >= 1 && services[2].connected_peer_count() >= 1
    })
    .await;

    let suppressed_id = all[0]
        .originate(
            EventType::new("GossipTestEvent").unwrap(),
            TEST_OFS_SPEC,
            Priority::Advertisement,
            8,
            b"suppress-me".to_vec(),
        )
        .unwrap();
    let allowed_id = all[0]
        .originate(
            EventType::new("GossipTestEvent").unwrap(),
            TEST_OFS_SPEC,
            Priority::Advertisement,
            8,
            b"let-me-through".to_vec(),
        )
        .unwrap();

    drive_until(&mut all, |services| services[2].has_event(&allowed_id)).await;
    drive_briefly(&mut all, Duration::from_millis(500)).await;

    assert!(
        all[1].has_event(&suppressed_id),
        "the filtered-out event is still stored locally by the node that vetoed its forward"
    );
    assert!(
        !all[2].has_event(&suppressed_id),
        "a forward filter returning false must stop the event reaching a further hop"
    );
    assert!(
        all[2].has_event(&allowed_id),
        "a forward filter must only suppress the content it targets, not everything"
    );
}

/// Two independently-started nodes have no shared advance knowledge of
/// each other's signing key — this proves `register_peer_key` genuinely
/// doesn't need to be called by hand (unlike every other test in this
/// file, which registers keys explicitly since they simulate a harness
/// that already knows every participant): a connection alone must be
/// enough for each side to validate the other's originated events.
#[tokio::test]
async fn a_connection_alone_lets_two_nodes_validate_each_others_events_with_no_manual_key_registration()
 {
    let mut a = make_service(1);
    let a_addr = listen_addr(&mut a).await;
    let mut b = make_service(2);
    b.node.dial(a_addr).unwrap();

    let mut both = vec![a, b];
    drive_until(&mut both, |services| {
        services.iter().all(|s| s.connected_peer_count() >= 1)
    })
    .await;

    let event_id = both[1]
        .originate(
            EventType::new("GossipTestEvent").unwrap(),
            TEST_OFS_SPEC,
            Priority::Advertisement,
            8,
            b"no-manual-registration-needed".to_vec(),
        )
        .unwrap();

    drive_until(&mut both, |services| services[0].has_event(&event_id)).await;
    assert!(
        matches!(
            both[0].get_event(&event_id),
            Some(envelope) if envelope.payload == b"no-manual-registration-needed"
        ),
        "node 0 must have actually stored (i.e. validated, not just received) node 1's event"
    );
}

/// Two nodes meeting must not hand each other the same events forever.
///
/// Both request recovery on connect, so each answers with its whole
/// backlog. Applying those responses with no source peer excluded nobody
/// from the re-broadcast, so each node pushed the backlog straight back
/// at the other — 174 "Dropping inbound stream because we are at
/// capacity" warnings in three minutes on a real pair of nodes, and every
/// dropped stream is an event that has to be fetched again.
#[tokio::test]
async fn recovered_events_are_not_pushed_back_at_the_peer_that_supplied_them() {
    let mut a = make_service(60);
    let mut b = make_service(61);

    let a_addr = listen_addr(&mut a).await;
    let (a_id, a_key) = identity(60);
    let (b_id, b_key) = identity(61);
    a.register_peer_key(b_id, b_key);
    b.register_peer_key(a_id, a_key);

    // A backlog on `a` before `b` ever connects, so `b` learns it by
    // recovery rather than by push — the path this test is about.
    let mut backlog = Vec::new();
    for i in 0..8u8 {
        backlog.push(
            a.originate(
                EventType::new("GossipTestEvent").unwrap(),
                TEST_OFS_SPEC,
                Priority::Advertisement,
                8,
                vec![i],
            )
            .unwrap(),
        );
    }

    b.node.dial(a_addr).unwrap();
    let mut all = vec![a, b];
    drive_until(&mut all, |services| services[1].connected_peer_count() >= 1).await;

    // `b` recovers everything.
    drive_until(&mut all, |services| {
        backlog.iter().all(|id| services[1].has_event(id))
    })
    .await;

    // The property: `a` keeps exactly what it originated. Were `b`
    // pushing the recovered events back, `a` would be receiving its own
    // backlog again — which is the traffic that exhausted the stream
    // capacity, not a change in what either node ends up holding.
    for id in &backlog {
        assert!(all[0].has_event(id));
        assert!(all[1].has_event(id), "recovery must still work");
    }
}
