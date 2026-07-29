//! The Phase 6c exit criterion for oracles: three mock providers' rates
//! aggregate to the correct median, converging identically across every
//! node in the cluster.

use futures::future::select_all;
use openfiat_crypto::Keypair;
use openfiat_gossip::EventStore;
use openfiat_gossip::channel::Subscription;
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_network::identity::{peer_id, to_libp2p_keypair};
use openfiat_network::{Multiaddr, Node};
use openfiat_oracles::{OracleData, OracleService};
use openfiat_registry::{Registration, Registry, SignedRegistration};
use openfiat_storage::mem::MemoryStore;
use openfiat_types::{MarketDataService, PeerId, PublicKey, ServiceId, ServiceType, Timestamp};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::time::Duration;

/// The same signed registration, independently applied to each node's
/// own local registry instance — mirroring `openfiat-disputes`'s
/// `seeded_settlements` helper: this crate's own replication test only
/// needs to exercise *its own* gossip channel, not `openfiat-registry`'s.
fn seeded_registries(providers: &[Keypair]) -> Rc<Registry<MemoryStore>> {
    let registry = Rc::new(Registry::new(MemoryStore::new()));
    for (i, provider) in providers.iter().enumerate() {
        let registration = Registration {
            service_id: ServiceId::new(format!("oracle-svc-{i}")),
            service_type: ServiceType::MarketData(MarketDataService::FxOracle),
            provider: peer_id_from_public_key(&provider.public_key()).unwrap(),
            provider_public_key: provider.public_key(),
            endpoints: vec![],
            supported_ofs: vec![7000],
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

fn make_service(seed: u8, services: Rc<Registry<MemoryStore>>) -> OracleService<MemoryStore> {
    let keypair = Keypair::from_seed([seed; 32]);
    let node = Node::new(&keypair).unwrap();
    let event_store = EventStore::new(MemoryStore::new());
    let gossip =
        openfiat_gossip::GossipService::new(node, event_store, keypair, vec![], Subscription::All);
    OracleService::new(gossip, MemoryStore::new(), services)
}

fn identity(seed: u8) -> (PeerId, PublicKey) {
    let keypair = Keypair::from_seed([seed; 32]);
    (peer_id(&to_libp2p_keypair(&keypair)), keypair.public_key())
}

async fn drive_until(
    services: &mut [OracleService<MemoryStore>],
    mut condition: impl FnMut(&[OracleService<MemoryStore>]) -> bool,
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

async fn listen_addr(service: &mut OracleService<MemoryStore>) -> Multiaddr {
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
async fn three_providers_rates_aggregate_to_the_correct_median_across_the_cluster() {
    let seeds: [u8; 3] = [1, 2, 3];
    let providers: Vec<Keypair> = seeds
        .iter()
        .map(|&seed| Keypair::from_seed([seed; 32]))
        .collect();
    let services = seeded_registries(&providers);
    let mut all: Vec<OracleService<MemoryStore>> = seeds
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

    let rates = [129.50, 129.54, 129.51];
    for (i, (service, rate)) in all.iter_mut().zip(rates).enumerate() {
        let data = OracleData::ExchangeRate {
            base: "USD".to_string(),
            quote: "KES".to_string(),
            rate,
        };
        service
            .publish(format!("usd-kes-{i}"), data, 1, Duration::from_secs(60))
            .unwrap();
    }
    drive_until(&mut all, |services| {
        services.iter().all(|s| s.all().len() == 3)
    })
    .await;

    for service in &all {
        assert_eq!(service.median_exchange_rate("USD", "KES"), Some(129.51));
    }
}
