//! What a node that lies, replays, floods or fabricates actually achieves.
//!
//! Every test here plays the attacker through the same entry points a
//! stranger has — a real connection and a real `GossipPush`, or
//! `receive_event`, which is what the wire handler calls — and asserts the
//! attack does not pay. None of them calls a validation helper directly:
//! a check nothing routes through is not a defence, and this workspace has
//! shipped that mistake often enough to test for it explicitly.
//!
//! The honest counterpart of each is in `propagation.rs`; the boundary
//! this cannot close is written down in `docs/dishonest-node.md`.

use futures::future::select_all;
use libp2p::swarm::SwarmEvent;
use openfiat_crypto::Keypair;
use openfiat_gossip::channel::Subscription;
use openfiat_gossip::error::GossipError;
use openfiat_gossip::protocol::{
    MESSAGE_TYPE_PUSH, MESSAGE_TYPE_RECOVERY_REQUEST, MESSAGE_TYPE_RECOVERY_RESPONSE, OFS_SPEC,
    RecoveryRequest, RecoveryResponse,
};
use openfiat_gossip::service::{MAX_REACHABLE_ADDRESSES, MAX_TTL, ReceiveOutcome};
use openfiat_gossip::{EventStore, GossipService, event_id};
use openfiat_network::behaviour::OpenFiatBehaviourEvent;
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_network::{Envelope, Multiaddr, Node, PeerId as Libp2pPeerId};
use openfiat_serialization::wire;
use openfiat_storage::mem::MemoryStore;
use openfiat_types::{EventEnvelope, EventId, EventType, Priority, Timestamp};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

const TEST_OFS_SPEC: u16 = 9999;

fn service(seed: u8) -> GossipService<MemoryStore> {
    let keypair = Keypair::from_seed([seed; 32]);
    GossipService::new(
        Node::new(&keypair).unwrap(),
        EventStore::new(MemoryStore::new()),
        keypair,
        vec![],
        Subscription::All,
    )
}

/// An event genuinely signed by `keypair` — everything an honest origin
/// produces, assembled by hand so a test can then change one field of it.
fn signed(keypair: &Keypair, payload: &[u8], ttl: u8, at: Timestamp) -> EventEnvelope {
    let origin = peer_id_from_public_key(&keypair.public_key()).unwrap();
    let event_type = EventType::new("GossipTestEvent").unwrap();
    let signable = event_id::signable_bytes(&event_type, TEST_OFS_SPEC, &origin, at, payload);
    let signature = keypair.sign(&signable);
    EventEnvelope {
        id: event_id::compute(&event_type, payload, at, &origin, &signature),
        event_type,
        ofs_spec: TEST_OFS_SPEC,
        version: 1,
        origin,
        timestamp: at,
        ttl,
        priority: Priority::Advertisement,
        signature,
        payload: payload.to_vec(),
    }
}

fn push_envelope(event: &EventEnvelope) -> Envelope {
    Envelope::new(
        OFS_SPEC,
        MESSAGE_TYPE_PUSH,
        1,
        wire::to_bytes(event).unwrap(),
    )
}

async fn listen_addr(service: &mut GossipService<MemoryStore>) -> Multiaddr {
    service
        .node
        .listen_on("/ip4/127.0.0.1/udp/0/quic-v1".parse().unwrap())
        .unwrap();
    loop {
        if let SwarmEvent::NewListenAddr { address, .. } = service.node.next_event().await {
            return address;
        }
    }
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

/// Drive an honest service and a raw attacker node together, collecting
/// every envelope the attacker gets back, until `enough` responses arrive
/// or the window closes.
async fn drive_pair(
    honest: &mut GossipService<MemoryStore>,
    attacker: &mut Node,
    window: Duration,
    enough: usize,
) -> Vec<Envelope> {
    let mut responses = Vec::new();
    let _ = tokio::time::timeout(window, async {
        loop {
            tokio::select! {
                event = honest.node.next_event() => honest.handle(event),
                event = attacker.next_event() => {
                    if let SwarmEvent::Behaviour(OpenFiatBehaviourEvent::Envelope(
                        libp2p::request_response::Event::Message { message, .. },
                    )) = event
                        && let libp2p::request_response::Message::Response { response, .. } = message
                    {
                        responses.push(response);
                        if responses.len() >= enough {
                            return;
                        }
                    }
                }
            }
        }
    })
    .await;
    responses
}

/// Connect a raw attacker node to an honest service and wait until the
/// honest side has the connection.
async fn connect(honest: &mut GossipService<MemoryStore>, attacker: &mut Node, addr: Multiaddr) {
    attacker.dial(addr).unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while honest.connected_peer_count() == 0 {
            tokio::select! {
                event = honest.node.next_event() => honest.handle(event),
                event = attacker.next_event() => { let _ = event; }
            }
        }
    })
    .await
    .expect("the attacker never connected");
}

/// The dedup key must be a function of the event, or it is a field the
/// sender fills in.
///
/// One genuine signature, sixty-four ids. Every copy carries a real
/// signature over real content — the signature does not cover the id — so
/// before the id was checked on arrival each one was a *distinct* event to
/// every peer: stored, handed to every domain handler, and re-forwarded.
/// One signed message became unbounded traffic and unbounded storage, and
/// nothing about it looked like a replay, because a replay is something
/// the store recognises by id.
#[tokio::test]
async fn one_signature_cannot_be_ground_into_many_events_by_rewriting_the_id() {
    let mut honest = service(40);
    let honest_addr = listen_addr(&mut honest).await;

    let attacker_keypair = Keypair::from_seed([41; 32]);
    let mut attacker = Node::new(&attacker_keypair).unwrap();
    connect(&mut honest, &mut attacker, honest_addr).await;
    let honest_peer = honest.node.libp2p_peer_id();

    let genuine = signed(&attacker_keypair, b"ground-me", 8, Timestamp::now());
    for i in 0..64u8 {
        let mut forged = genuine.clone();
        forged.id = EventId::from_bytes([i; 32]);
        attacker.send_envelope(honest_peer, push_envelope(&forged));
    }
    // The unmodified event last, so reaching *it* proves the sixty-four
    // ahead of it were seen and refused rather than still in flight.
    attacker.send_envelope(honest_peer, push_envelope(&genuine));

    tokio::time::timeout(Duration::from_secs(15), async {
        while !honest.has_event(&genuine.id) {
            tokio::select! {
                event = honest.node.next_event() => honest.handle(event),
                event = attacker.next_event() => { let _ = event; }
            }
        }
    })
    .await
    .expect("the honest node never accepted the genuine event");

    assert_eq!(
        honest.event_count(),
        1,
        "sixty-four rewritten ids over one signature must produce one event, not sixty-five"
    );
}

/// The same, stated at the outcome level: a rewritten id is rejected, and
/// rejected for being a rewritten id rather than incidentally.
#[test]
fn an_event_whose_id_is_not_its_own_content_is_refused() {
    let mut honest = service(42);
    let stranger = Keypair::from_seed([43; 32]);

    let mut forged = signed(&stranger, b"not-my-id", 8, Timestamp::now());
    forged.id = EventId::from_bytes([9; 32]);

    assert_eq!(
        honest.receive_event(None, forged.clone()),
        ReceiveOutcome::Rejected(GossipError::EventIdMismatch)
    );
    assert!(!honest.has_event(&forged.id));
    assert_eq!(honest.event_count(), 0);
}

/// A far-future stamp is a permanent row in a log that is pruned by
/// timestamp and nothing else.
#[test]
fn an_event_stamped_past_any_plausible_clock_is_refused() {
    let mut honest = service(44);
    let stranger = Keypair::from_seed([45; 32]);

    let a_year_out = Timestamp::from_millis(Timestamp::now().as_millis() + 365 * 86_400_000);
    let unprunable = signed(&stranger, b"see-you-next-year", 8, a_year_out);

    assert_eq!(
        honest.receive_event(None, unprunable.clone()),
        ReceiveOutcome::Rejected(GossipError::TimestampTooFarAhead)
    );
    assert!(
        !honest.has_event(&unprunable.id),
        "no sweep computed from wall-clock time would ever reach this event again"
    );
}

/// A clock that is merely wrong, rather than lying, is still tolerated —
/// the cost of the bound above, held to a bound of its own.
#[test]
fn a_slightly_fast_clock_is_still_believed() {
    let mut honest = service(46);
    let stranger = Keypair::from_seed([47; 32]);

    let a_minute_out = Timestamp::from_millis(Timestamp::now().as_millis() + 60_000);
    let event = signed(&stranger, b"my-ntp-is-off", 8, a_minute_out);

    assert_eq!(
        honest.receive_event(None, event),
        ReceiveOutcome::Stored,
        "refusing honest traffic over a minute of clock drift would cost more than it saves"
    );
}

/// TTL is the one field the protocol expects to change in flight, so it is
/// outside the signature, so any relay can write anything into it.
///
/// Both halves of the answer are asserted here, because getting the
/// direction wrong turns the check into the attack: an inflated budget is
/// cut back to the protocol's, and the event is still *accepted*. Refusing
/// it would let any relay destroy someone else's signed event by raising a
/// number nobody signed.
#[test]
fn an_inflated_hop_budget_is_cut_back_rather_than_used_to_censor() {
    let mut honest = service(48);
    let stranger = Keypair::from_seed([49; 32]);
    let greedy = signed(&stranger, b"255-hops-please", u8::MAX, Timestamp::now());

    assert_eq!(
        honest.receive_event(None, greedy.clone()),
        ReceiveOutcome::Stored,
        "an event must not become droppable by everyone just because a relay touched its ttl"
    );
    assert_eq!(
        honest.get_event(&greedy.id).unwrap().ttl,
        MAX_TTL,
        "the next hop must be handed this node's budget, not a stranger's"
    );
}

/// Replay: the same signed event, pushed again by anyone who captured it.
///
/// The property that matters is not that the second copy is refused — it
/// is that domain handlers are notified once, because that notification is
/// what applies the event to a registry.
#[tokio::test]
async fn a_captured_event_re_pushed_is_not_applied_a_second_time() {
    let mut honest = service(50);
    let honest_addr = listen_addr(&mut honest).await;

    let applications = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let counter = applications.clone();
    honest.add_event_handler(move |_| counter.set(counter.get() + 1));

    let attacker_keypair = Keypair::from_seed([51; 32]);
    let mut attacker = Node::new(&attacker_keypair).unwrap();
    connect(&mut honest, &mut attacker, honest_addr).await;
    let honest_peer = honest.node.libp2p_peer_id();

    let captured = signed(&attacker_keypair, b"replay-me", 8, Timestamp::now());
    for _ in 0..16 {
        attacker.send_envelope(honest_peer, push_envelope(&captured));
    }

    tokio::time::timeout(Duration::from_secs(15), async {
        while !honest.has_event(&captured.id) {
            tokio::select! {
                event = honest.node.next_event() => honest.handle(event),
                event = attacker.next_event() => { let _ = event; }
            }
        }
    })
    .await
    .expect("the honest node never accepted the event");
    let _ = drive_pair(&mut honest, &mut attacker, Duration::from_millis(400), 999).await;

    assert_eq!(honest.event_count(), 1);
    assert_eq!(
        applications.get(),
        1,
        "sixteen deliveries of one signed event must reach the registries once"
    );
}

/// A recovery request is a few dozen bytes and its answer is as much of
/// the event log as fits in an envelope.
///
/// Answering every one of them makes this node an amplifier that any
/// connected peer can aim at itself for free, in a loop. The honest
/// protocol asks once per connection, so that is what is served; the rest
/// are answered with nothing, which still frees the sender's inbound
/// stream slot.
#[tokio::test]
async fn a_flood_of_recovery_requests_is_answered_once_with_the_log_and_then_with_nothing() {
    let mut honest = service(52);
    for i in 0..8u8 {
        honest
            .originate(
                EventType::new("GossipTestEvent").unwrap(),
                TEST_OFS_SPEC,
                Priority::Advertisement,
                8,
                vec![i],
            )
            .unwrap();
    }
    let honest_addr = listen_addr(&mut honest).await;

    let mut attacker = Node::new(&Keypair::from_seed([53; 32])).unwrap();
    connect(&mut honest, &mut attacker, honest_addr).await;
    let honest_peer = honest.node.libp2p_peer_id();

    let request = wire::to_bytes(&RecoveryRequest {
        subscription: Subscription::All,
    })
    .unwrap();
    const ASKS: usize = 20;
    for _ in 0..ASKS {
        attacker.send_envelope(
            honest_peer,
            Envelope::new(OFS_SPEC, MESSAGE_TYPE_RECOVERY_REQUEST, 1, request.clone()),
        );
    }

    let responses = drive_pair(&mut honest, &mut attacker, Duration::from_secs(10), ASKS).await;
    let recoveries: Vec<RecoveryResponse> = responses
        .iter()
        .filter(|envelope| envelope.header.message_type == MESSAGE_TYPE_RECOVERY_RESPONSE)
        .map(|envelope| wire::from_bytes(&envelope.payload).unwrap())
        .collect();

    assert_eq!(
        recoveries.len(),
        ASKS,
        "every request must be answered — an unanswered one holds an inbound stream slot \
         until it times out, which is a cheaper flood than this one"
    );
    let carrying_events = recoveries.iter().filter(|r| !r.events.is_empty()).count();
    assert_eq!(
        carrying_events, 1,
        "twenty asks must move the log across the wire once"
    );
    assert_eq!(
        recoveries
            .iter()
            .find(|r| !r.events.is_empty())
            .unwrap()
            .events
            .len(),
        8
    );
}

/// A node whose log outgrew one envelope used to answer recovery with
/// silence.
///
/// `MAX_ENVELOPE_BYTES` is a hard 1 MiB and the codec refuses to *write*
/// anything larger, so building the whole log and handing it to
/// `send_response` failed to encode and sent nothing — not a truncated
/// answer, no answer. Every node past a megabyte of history, which is
/// every node that has been up a day, had silently stopped being able to
/// bootstrap anyone. Withholding is supposed to be a choice a dishonest
/// node makes; this was every honest node doing it by accident.
#[tokio::test]
async fn a_log_too_large_for_one_envelope_is_answered_in_part_rather_than_not_at_all() {
    let mut honest = service(62);
    let bulk = vec![0xab; 32 * 1024];
    for i in 0..40u8 {
        let mut payload = bulk.clone();
        payload[0] = i;
        honest
            .originate(
                EventType::new("GossipTestEvent").unwrap(),
                TEST_OFS_SPEC,
                Priority::Advertisement,
                8,
                payload,
            )
            .unwrap();
    }
    let total = honest.event_count();
    let honest_addr = listen_addr(&mut honest).await;

    let mut peer = Node::new(&Keypair::from_seed([63; 32])).unwrap();
    connect(&mut honest, &mut peer, honest_addr).await;
    let honest_peer = honest.node.libp2p_peer_id();
    peer.send_envelope(
        honest_peer,
        Envelope::new(
            OFS_SPEC,
            MESSAGE_TYPE_RECOVERY_REQUEST,
            1,
            wire::to_bytes(&RecoveryRequest {
                subscription: Subscription::All,
            })
            .unwrap(),
        ),
    );

    let responses = drive_pair(&mut honest, &mut peer, Duration::from_secs(10), 1).await;
    let recovered: RecoveryResponse = responses
        .iter()
        .find(|envelope| envelope.header.message_type == MESSAGE_TYPE_RECOVERY_RESPONSE)
        .map(|envelope| wire::from_bytes(&envelope.payload).unwrap())
        .expect("a log larger than one envelope must still produce an answer");

    assert!(
        !recovered.events.is_empty(),
        "partial catch-up beats none: the rest is what snapshots are for"
    );
    assert!(
        recovered.events.len() < total,
        "this log genuinely does not fit in one envelope, or the test proves nothing"
    );
    assert!(
        recovered
            .events
            .windows(2)
            .all(|pair| pair[0].timestamp <= pair[1].timestamp),
        "oldest first, so the requester makes contiguous progress through its gap"
    );
}

/// An inbound request this node has no handler for is still an inbound
/// request holding a stream slot.
///
/// Dropping the channel costs the sender nothing and costs this node the
/// slot until the timeout — a flood anyone can mount with a typo, and one
/// this codebase already hit once through the push path.
#[tokio::test]
async fn a_gossip_message_type_this_node_does_not_implement_is_still_answered() {
    let mut honest = service(54);
    let honest_addr = listen_addr(&mut honest).await;

    let mut attacker = Node::new(&Keypair::from_seed([55; 32])).unwrap();
    connect(&mut honest, &mut attacker, honest_addr).await;
    let honest_peer = honest.node.libp2p_peer_id();

    for _ in 0..8 {
        attacker.send_envelope(
            honest_peer,
            Envelope::new(OFS_SPEC, "GossipNonsense", 1, vec![0xff; 64]),
        );
    }

    let responses = drive_pair(&mut honest, &mut attacker, Duration::from_secs(10), 8).await;
    assert_eq!(
        responses.len(),
        8,
        "an unhandled message type must free the slot it occupies"
    );
}

/// identify's `observed_addr` is free text on the far side of the
/// connection, and this node keeps what it is told there.
///
/// One peer, one fresh address per report, forever: before the cap this
/// was a set that grew for as long as an attacker felt like typing. The
/// number of true answers is the number of interfaces this host has.
#[tokio::test]
async fn a_peer_inventing_a_new_address_every_time_cannot_grow_this_node_without_bound() {
    let mut honest = service(56);
    let liar = Libp2pPeerId::random();

    for i in 0..(MAX_REACHABLE_ADDRESSES * 4) {
        let claimed: Multiaddr = format!("/ip4/198.51.100.{}/tcp/{}", i % 256, 4000 + i / 256)
            .parse()
            .unwrap();
        honest.handle_lifecycle(&identify_event(liar, claimed));
    }

    assert!(
        honest.reachable_addresses().len() <= MAX_REACHABLE_ADDRESSES,
        "the set a stranger fills must have a ceiling"
    );
    assert!(
        honest.corroborated_addresses().is_empty(),
        "and none of it may be acted on: one peer is still one peer, however many \
         addresses it names"
    );
}

/// Building the identify event a peer's claim actually arrives in, so the
/// test above goes through `handle_lifecycle` rather than reaching past it.
fn identify_event(
    peer_id: Libp2pPeerId,
    observed_addr: Multiaddr,
) -> SwarmEvent<OpenFiatBehaviourEvent> {
    let public_key =
        openfiat_network::identity::to_libp2p_keypair(&Keypair::from_seed([99; 32])).public();
    SwarmEvent::Behaviour(OpenFiatBehaviourEvent::Identify(
        libp2p::identify::Event::Received {
            connection_id: libp2p::swarm::ConnectionId::new_unchecked(0),
            peer_id,
            info: libp2p::identify::Info {
                public_key,
                protocol_version: "openfiat/1".to_string(),
                agent_version: "test".to_string(),
                listen_addrs: Vec::new(),
                protocols: Vec::new(),
                observed_addr,
                signed_peer_record: None,
            },
        },
    ))
}

/// Epidemic propagation past one hop, with nobody's key registered by
/// hand.
///
/// Not an attack — a bug the attacks uncovered. Keys used to be cached
/// from `ConnectionEstablished`, so a node could verify its direct peers
/// and nobody else, and an event relayed two hops named an origin it had
/// no key for: `InvalidSignature`, silently, on every real deployment.
/// Every other test in this crate registered the whole cluster's keys up
/// front and so could not see it. This one registers nothing.
#[tokio::test]
async fn an_event_relayed_from_an_origin_this_node_never_connected_to_still_validates() {
    let mut x = service(57);
    let mut y = service(58);
    let mut z = service(59);

    let x_addr = listen_addr(&mut x).await;
    y.node.dial(x_addr).unwrap();
    let y_addr = listen_addr(&mut y).await;
    z.node.dial(y_addr).unwrap();

    let mut chain = vec![x, y, z];
    drive_until(&mut chain, |s| {
        s[1].connected_peer_count() >= 1 && s[2].connected_peer_count() >= 1
    })
    .await;

    let id = chain[0]
        .originate(
            EventType::new("GossipTestEvent").unwrap(),
            TEST_OFS_SPEC,
            Priority::Advertisement,
            8,
            b"two-hops-away".to_vec(),
        )
        .unwrap();

    drive_until(&mut chain, |s| s[2].has_event(&id)).await;
    drive_briefly(&mut chain, Duration::from_millis(200)).await;

    assert!(
        chain[2].has_event(&id),
        "z has never connected to x, and must still be able to check x's signature"
    );
}
