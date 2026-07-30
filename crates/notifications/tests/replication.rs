//! The notification delivery path across a real gossip cluster: a
//! wallet's destination-bearing subscription replicates, every node then
//! independently plans the same delivery under the same deterministic
//! id, and only then does the bound gateway's signed delivery report
//! replicate and stick.
//!
//! The deterministic-id assertion here is the load-bearing one. Each of
//! these nodes runs its own dispatcher over the same replicated state;
//! if they disagreed on the id, the gateway would have no way to tell N
//! copies of one notification from N distinct ones, and the recipient
//! would get a message per node.

use futures::future::select_all;
use openfiat_crypto::Keypair;
use openfiat_gossip::EventStore;
use openfiat_gossip::channel::Subscription as GossipSubscription;
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_network::identity::{peer_id, to_libp2p_keypair};
use openfiat_network::{Multiaddr, Node};
use openfiat_notifications::{
    DeliveryStatus, NotificationCategory, NotificationId, NotificationService, NotificationTrigger,
    SubscriptionDestination,
};
use openfiat_registry::{Registration, Registry, SignedRegistration};
use openfiat_storage::mem::MemoryStore;
use openfiat_types::{NotificationChannel, PeerId, PublicKey, ServiceId, ServiceType, Timestamp};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::time::Duration;

/// The same signed registration, independently applied to each node's own
/// local registry instance — mirroring `openfiat-disputes`'s
/// `seeded_settlements` helper: this crate's own replication test only
/// needs to exercise *its own* gossip channel, not `openfiat-registry`'s.
fn seeded_registry(provider: &Keypair, service_id: &str) -> Rc<Registry<MemoryStore>> {
    let registry = Rc::new(Registry::new(MemoryStore::new()));
    let registration = Registration {
        service_id: ServiceId::new(service_id),
        service_type: ServiceType::Notifications(NotificationChannel::Sms),
        provider: peer_id_from_public_key(&provider.public_key()).unwrap(),
        provider_public_key: provider.public_key(),
        endpoints: vec!["https://sms.example.com/webhook".to_string()],
        supported_ofs: vec![6000],
        region: None,
        capabilities: vec![],
        pricing: None,
        payout_wallet: None,
        timestamp: Timestamp::now(),
    };
    registry
        .apply_registration(SignedRegistration::sign(registration, provider))
        .unwrap();
    registry
}

fn make_service(seed: u8, services: Rc<Registry<MemoryStore>>) -> NotificationService<MemoryStore> {
    let keypair = Keypair::from_seed([seed; 32]);
    let node = Node::new(&keypair).unwrap();
    let event_store = EventStore::new(MemoryStore::new());
    let gossip = openfiat_gossip::GossipService::new(
        node,
        event_store,
        keypair,
        vec![],
        GossipSubscription::All,
    );
    NotificationService::new(gossip, MemoryStore::new(), services)
}

fn identity(seed: u8) -> (PeerId, PublicKey) {
    let keypair = Keypair::from_seed([seed; 32]);
    (peer_id(&to_libp2p_keypair(&keypair)), keypair.public_key())
}

async fn drive_until(
    services: &mut [NotificationService<MemoryStore>],
    mut condition: impl FnMut(&[NotificationService<MemoryStore>]) -> bool,
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

async fn listen_addr(service: &mut NotificationService<MemoryStore>) -> Multiaddr {
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
async fn a_subscription_and_a_delivery_report_replicate_across_the_cluster() {
    let seeds: [u8; 2] = [1, 2]; // wallet + provider
    let provider_keypair = Keypair::from_seed([2u8; 32]);
    let registries: Vec<_> = seeds
        .iter()
        .map(|_| seeded_registry(&provider_keypair, "svc-1"))
        .collect();
    let mut all: Vec<NotificationService<MemoryStore>> = seeds
        .iter()
        .zip(registries.iter())
        .map(|(&seed, registry)| make_service(seed, Rc::clone(registry)))
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

    let wallet_peer_id = identities[0].0.clone();
    // Sealed to the gateway's own registered public key, so nothing on
    // the wire — and nothing in any node's replica — is the address.
    let destination = SubscriptionDestination {
        service_id: ServiceId::new("svc-1"),
        channel: NotificationChannel::Sms,
        sealed: openfiat_crypto::seal(&provider_keypair.public_key(), b"+254700000000").unwrap(),
    };
    all[0]
        .update_subscription(vec![NotificationCategory::Trading], vec![destination])
        .unwrap();
    drive_until(&mut all, |services| {
        services
            .iter()
            .all(|s| s.subscription(&wallet_peer_id).is_some())
    })
    .await;
    for service in &all {
        assert!(
            service
                .subscription(&wallet_peer_id)
                .unwrap()
                .wants(NotificationTrigger::TradeCompleted)
        );
        assert!(
            !service
                .subscription(&wallet_peer_id)
                .unwrap()
                .wants(NotificationTrigger::SnapshotAvailable)
        );
    }

    // Every node plans independently off its own replica — no shared
    // state, no coordination — and must agree byte for byte on the id.
    let source_event = b"settlement-event-id";
    let planned: Vec<_> = all
        .iter()
        .map(|service| {
            let plan = service.plan(
                NotificationTrigger::TradeCompleted,
                source_event,
                &wallet_peer_id,
            );
            assert_eq!(
                plan.skipped,
                vec![],
                "the gateway is registered and healthy"
            );
            assert_eq!(plan.deliveries.len(), 1);
            assert_eq!(
                plan.deliveries[0].endpoint,
                "https://sms.example.com/webhook"
            );
            plan.deliveries.into_iter().next().unwrap()
        })
        .collect();
    let notification_id = planned[0].payload.notification_id.clone();
    for delivery in &planned {
        assert_eq!(
            delivery.payload.notification_id, notification_id,
            "two nodes dispatching the same event must mint the same id"
        );
    }
    assert_eq!(
        notification_id,
        NotificationId::derive(
            NotificationTrigger::TradeCompleted,
            source_event,
            &wallet_peer_id
        )
    );
    // Only the bound gateway can read the destination this carries.
    assert_eq!(
        openfiat_crypto::open(&provider_keypair, &planned[0].payload.sealed_destination).unwrap(),
        b"+254700000000"
    );

    // Each node records its own handoff before any report is accepted —
    // that record is what makes the gateway's report checkable.
    for (service, delivery) in all.iter().zip(planned.iter()) {
        service.record_queued(delivery);
        assert_eq!(
            service.dispatch(&notification_id).unwrap().status,
            DeliveryStatus::Queued
        );
        service.record_handoff(&notification_id, true);
        assert_eq!(
            service.dispatch(&notification_id).unwrap().status,
            DeliveryStatus::Sent,
            "a node can honestly witness the handoff and nothing beyond it"
        );
    }

    all[1]
        .report_delivery(
            notification_id.clone(),
            ServiceId::new("svc-1"),
            wallet_peer_id.clone(),
            NotificationTrigger::TradeCompleted,
            DeliveryStatus::Delivered,
        )
        .unwrap();
    drive_until(&mut all, |services| {
        services
            .iter()
            .all(|s| s.receipt(&notification_id).is_some())
    })
    .await;
    for service in &all {
        let receipt = service.receipt(&notification_id).unwrap();
        assert_eq!(receipt.status, DeliveryStatus::Delivered);
        assert_eq!(service.receipts_for(&wallet_peer_id).len(), 1);
        assert_eq!(
            service.dispatch(&notification_id).unwrap().status,
            DeliveryStatus::Sent,
            "the gateway's last-mile claim must not overwrite what the node itself observed"
        );
    }
}
