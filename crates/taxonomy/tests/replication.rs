//! A payment method one merchant defines reaches a node they have never
//! spoken to.
//!
//! This is the test the feature exists for. A merchant "adding" a payment
//! method used to mean a row in one browser's `localStorage`, under a
//! footnote claiming it had been shared; the counterparty who had to pay
//! it saw an advertisement naming something nothing could resolve. So the
//! claim being checked here is not "the registry stores records" — the
//! unit tests cover that — but that a definition signed on one node is
//! readable, by name, on a node two hops away.
//!
//! # Why the topology is a line and not a triangle
//!
//! The three nodes are wired `merchant — relay — counterparty`, and the
//! two ends never connect to each other. That is the case that matters: a
//! buyer browsing an order book has no connection to the merchant who
//! wrote it, and until relayed events were verified from a peer id rather
//! than a hand-registered key, a test that connected everybody to
//! everybody would have passed without ever exercising a relay.
//!
//! Nothing here registers a peer key by hand, for the same reason.

use futures::future::select_all;
use openfiat_crypto::Keypair;
use openfiat_gossip::channel::Subscription;
use openfiat_gossip::{EventStore, GossipService};
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_network::{Multiaddr, Node};
use openfiat_storage::mem::MemoryStore;
use openfiat_taxonomy::{
    MerchantPaymentMethod, PaymentMethodCategory, PaymentMethodRef, PaymentMethodRegistry,
    SignedPaymentMethodDefine, protocol,
};
use openfiat_types::{EventType, NodeRole, Priority};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::time::Duration;

/// One node: a gossip service with a payment-method registry attached to
/// it, which is exactly how `openfiat_rpc::state::NodeState` wires the
/// two together.
struct TestNode {
    gossip: GossipService<MemoryStore>,
    methods: Rc<PaymentMethodRegistry<MemoryStore>>,
}

fn make_node(seed: u8) -> TestNode {
    let keypair = Keypair::from_seed([seed; 32]);
    let node = Node::new(&keypair).expect("a seeded key always builds a node");
    let mut gossip = GossipService::new(
        node,
        EventStore::new(MemoryStore::new()),
        keypair,
        vec![NodeRole::FullNode],
        Subscription::All,
    );
    let methods = Rc::new(PaymentMethodRegistry::new(MemoryStore::new()));
    let for_handler = Rc::clone(&methods);
    gossip.add_event_handler(move |event| for_handler.apply_event(event));
    TestNode { gossip, methods }
}

async fn listen_addr(node: &mut TestNode) -> Multiaddr {
    node.gossip
        .node
        .listen_on("/ip4/127.0.0.1/udp/0/quic-v1".parse().unwrap())
        .unwrap();
    loop {
        if let libp2p::swarm::SwarmEvent::NewListenAddr { address, .. } =
            node.gossip.node.next_event().await
        {
            return address;
        }
    }
}

async fn drive_until(nodes: &mut [TestNode], mut condition: impl FnMut(&[TestNode]) -> bool) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !condition(nodes) {
            let futures: Vec<Pin<Box<dyn Future<Output = ()> + '_>>> = nodes
                .iter_mut()
                .map(|n| Box::pin(n.gossip.drive_once()) as Pin<Box<dyn Future<Output = ()> + '_>>)
                .collect();
            select_all(futures).await;
        }
    })
    .await
    .expect("the definition did not reach every node in time")
}

#[tokio::test]
async fn a_merchants_own_payment_method_reaches_a_node_it_never_talked_to() {
    // The relay listens; the merchant and the counterparty each dial it,
    // and never each other.
    let mut relay = make_node(2);
    let relay_addr = listen_addr(&mut relay).await;

    let mut merchant_node = make_node(1);
    let mut counterparty = make_node(3);
    for end in [&mut merchant_node, &mut counterparty] {
        end.gossip.node.dial(relay_addr.clone()).unwrap();
    }

    let mut all = vec![merchant_node, relay, counterparty];
    drive_until(&mut all, |nodes| {
        nodes[1].gossip.connected_peer_count() >= 2
    })
    .await;

    let merchant = Keypair::from_seed([1u8; 32]);
    let definition = MerchantPaymentMethod {
        merchant: peer_id_from_public_key(&merchant.public_key()).unwrap(),
        merchant_public_key: merchant.public_key(),
        name: "Sacco Standing Order".to_string(),
        category: PaymentMethodCategory::BankTransfer,
    };
    let id = definition.id();
    let signed = SignedPaymentMethodDefine::sign(definition, &merchant);
    let payload =
        openfiat_serialization::wire::to_bytes(&signed).expect("a definition always serializes");

    all[0]
        .methods
        .apply_define(signed)
        .expect("the author's own node applies it directly");
    all[0]
        .gossip
        .originate(
            EventType::new(protocol::EVENT_DEFINED).unwrap(),
            protocol::OFS_SPEC,
            Priority::Reputation,
            8,
            payload,
        )
        .expect("any node may originate a definition of its own");

    drive_until(&mut all, |nodes| {
        nodes.iter().all(|n| n.methods.get(&id).is_some())
    })
    .await;

    // The counterparty is two hops from the merchant and can now turn the
    // id on an advertisement into a name.
    let seen = all[2]
        .methods
        .get(&id)
        .expect("the far end holds the definition");
    assert_eq!(seen.name, "Sacco Standing Order");
    assert_eq!(seen.published().id, id);

    // And it is still the merchant's alone to offer. A relay holding the
    // record does not acquire the right to advertise it.
    let relay_identity =
        peer_id_from_public_key(&Keypair::from_seed([2u8; 32]).public_key()).unwrap();
    assert!(id.is_selectable_by(&seen.merchant));
    assert!(
        !id.is_selectable_by(&relay_identity),
        "replicating a definition must not make it anybody's to choose"
    );

    // A built-in needs no replication at all: every node already has it.
    let builtin = PaymentMethodRef::builtin("mpesa-kenya").unwrap();
    assert!(
        openfiat_taxonomy::catalog()
            .iter()
            .any(|method| method.id == builtin)
    );
}
