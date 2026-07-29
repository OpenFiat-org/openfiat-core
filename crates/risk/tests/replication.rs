//! The Phase 6c exit criterion for risk: a screening query against two
//! mock providers producing the documented aggregate outcome — OFS-7100
//! §13's own worked example (Provider A: Scam Wallet, Provider B: Scam
//! Wallet, Provider C: No Record → Reject Deposit), converging
//! identically across every node in the cluster.

use futures::future::select_all;
use openfiat_crypto::Keypair;
use openfiat_gossip::EventStore;
use openfiat_gossip::channel::Subscription;
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_network::identity::{peer_id, to_libp2p_keypair};
use openfiat_network::{Multiaddr, Node};
use openfiat_registry::{Registration, Registry, SignedRegistration};
use openfiat_risk::{Confidence, ProviderCategory, RiskOutcome, RiskService, Severity};
use openfiat_storage::mem::MemoryStore;
use openfiat_types::{
    NodeRole, PeerId, PublicKey, SecurityService, ServiceId, ServiceType, Timestamp,
};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::time::Duration;

/// The same signed registrations, independently applied to each node's
/// own local registry instance — mirroring `openfiat-disputes`'s
/// `seeded_settlements` helper: this crate's own replication test only
/// needs to exercise *its own* gossip channel, not `openfiat-registry`'s.
fn seeded_registries(providers: &[Keypair]) -> Rc<Registry<MemoryStore>> {
    let registry = Rc::new(Registry::new(MemoryStore::new()));
    for (i, provider) in providers.iter().enumerate() {
        let registration = Registration {
            service_id: ServiceId::new(format!("risk-svc-{i}")),
            service_type: ServiceType::Security(SecurityService::RiskIntelligenceProvider),
            provider: peer_id_from_public_key(&provider.public_key()).unwrap(),
            provider_public_key: provider.public_key(),
            endpoints: vec![],
            supported_ofs: vec![7100],
            region: None,
            capabilities: vec![],
            pricing: None,
            payout_wallet: None,
            timestamp: Timestamp::now(),
        };
        registry
            .apply_registration(SignedRegistration::sign(registration, provider))
            .unwrap();
    }
    registry
}

fn make_service(seed: u8, services: Rc<Registry<MemoryStore>>) -> RiskService<MemoryStore> {
    let keypair = Keypair::from_seed([seed; 32]);
    let node = Node::new(&keypair).unwrap();
    let event_store = EventStore::new(MemoryStore::new());
    // §7 (OGP): only a node holding `RiskIntelligenceProvider` may
    // originate `WalletFlagged` events — see `authorization::is_authorized`.
    let gossip = openfiat_gossip::GossipService::new(
        node,
        event_store,
        keypair,
        vec![NodeRole::RiskIntelligenceProvider],
        Subscription::All,
    );
    RiskService::new(gossip, MemoryStore::new(), services)
}

fn identity(seed: u8) -> (PeerId, PublicKey) {
    let keypair = Keypair::from_seed([seed; 32]);
    (peer_id(&to_libp2p_keypair(&keypair)), keypair.public_key())
}

async fn drive_until(
    services: &mut [RiskService<MemoryStore>],
    mut condition: impl FnMut(&[RiskService<MemoryStore>]) -> bool,
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

async fn listen_addr(service: &mut RiskService<MemoryStore>) -> Multiaddr {
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
async fn two_of_three_providers_flagging_a_scam_wallet_aggregates_to_reject() {
    let seeds: [u8; 3] = [1, 2, 3]; // provider A, provider B, provider C (no record)
    let providers: Vec<Keypair> = seeds
        .iter()
        .map(|&seed| Keypair::from_seed([seed; 32]))
        .collect();
    let services = seeded_registries(&providers);
    let mut all: Vec<RiskService<MemoryStore>> = seeds
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

    let wallet = peer_id_from_public_key(&Keypair::generate().public_key()).unwrap();

    // Provider A flags the wallet as a known scam wallet (§13).
    all[0]
        .publish(
            "r-a",
            wallet.clone(),
            ProviderCategory::FraudIntelligence,
            RiskOutcome::Flagged,
            Severity::High,
            Confidence::High,
            "Known scam wallet",
            vec![],
            None,
        )
        .unwrap();
    // Provider B independently reaches the same conclusion.
    all[1]
        .publish(
            "r-b",
            wallet.clone(),
            ProviderCategory::FraudIntelligence,
            RiskOutcome::Flagged,
            Severity::High,
            Confidence::High,
            "Known scam wallet",
            vec![],
            None,
        )
        .unwrap();
    // Provider C never publishes anything for this wallet — "No Record".

    drive_until(&mut all, |services| {
        services.iter().all(|s| s.for_wallet(&wallet).len() == 2)
    })
    .await;

    for service in &all {
        let result = service.screen(&wallet);
        assert_eq!(result.highest_severity, Some(Severity::High));
        assert_eq!(
            result.active_flags.len(),
            2,
            "both providers' flags should be visible on every node, including provider C's"
        );
    }
}
