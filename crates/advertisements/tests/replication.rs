//! An advertisement created on one node replicates to the rest of a
//! gossip cluster, and a subsequent disable does too — proving the "ads
//! backend" actually works across the network, not just against a local
//! index (OFS-2100 §8, §23).

use futures::future::select_all;
use openfiat_advertisements::{
    AdvertisementId, AdvertisementService, AdvertisementStatus, Direction, PricingModel,
};
use openfiat_crypto::{Keypair, MintAddress};
use openfiat_gossip::EventStore;
use openfiat_gossip::channel::Subscription;
use openfiat_network::identity::{peer_id, to_libp2p_keypair};
use openfiat_network::{Multiaddr, Node};
use openfiat_storage::mem::MemoryStore;
use openfiat_types::{Amount, FiatCurrency, NodeRole, PeerId, PublicKey};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

fn make_service(seed: u8) -> AdvertisementService<MemoryStore> {
    let keypair = Keypair::from_seed([seed; 32]);
    let node = Node::new(&keypair).unwrap();
    let event_store = EventStore::new(MemoryStore::new());
    let gossip = openfiat_gossip::GossipService::new(
        node,
        event_store,
        keypair,
        vec![NodeRole::MerchantGateway],
        Subscription::All,
    );
    AdvertisementService::new(gossip, MemoryStore::new())
}

fn identity(seed: u8) -> (PeerId, PublicKey) {
    let keypair = Keypair::from_seed([seed; 32]);
    (peer_id(&to_libp2p_keypair(&keypair)), keypair.public_key())
}

async fn drive_until(
    services: &mut [AdvertisementService<MemoryStore>],
    mut condition: impl FnMut(&[AdvertisementService<MemoryStore>]) -> bool,
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

async fn listen_addr(service: &mut AdvertisementService<MemoryStore>) -> Multiaddr {
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
async fn a_created_and_then_disabled_advertisement_replicates_to_the_whole_cluster() {
    let mut hub = make_service(1);
    let hub_addr = listen_addr(&mut hub).await;

    let mut leaves: Vec<AdvertisementService<MemoryStore>> = (2..=3).map(make_service).collect();
    for leaf in &mut leaves {
        leaf.gossip.node.dial(hub_addr.clone()).unwrap();
    }

    let mut all: Vec<AdvertisementService<MemoryStore>> =
        std::iter::once(hub).chain(leaves).collect();

    let seeds = [1u8, 2, 3];
    let identities: Vec<_> = seeds.iter().map(|&seed| identity(seed)).collect();
    for (i, service) in all.iter_mut().enumerate() {
        for (j, (peer_id, public_key)) in identities.iter().enumerate() {
            if i != j {
                service
                    .gossip
                    .register_peer_key(peer_id.clone(), *public_key);
            }
        }
    }

    drive_until(&mut all, |services| {
        services
            .iter()
            .all(|s| s.gossip.connected_peer_count() >= 1)
    })
    .await;

    let ad_id = AdvertisementId::new("sell-usdc-kes-1");
    all[1]
        .create(
            "sell-usdc-kes-1",
            MintAddress::parse("2bHPi5hA4zrmPAfrvLmEexg3KJjpTjNkUcxWnzUPeRRU").unwrap(),
            Direction::Sell,
            FiatCurrency::parse("KES").unwrap(),
            Amount::new(1_000_000, 6),
            Amount::new(1_000_000_000, 6),
            Amount::new(10_000_000_000, 6),
            PricingModel::Fixed {
                price: Amount::new(129_000_000, 6),
            },
            vec!["Mobile Money".to_string()],
        )
        .unwrap();

    drive_until(&mut all, |services| {
        services.iter().all(|s| s.get(&ad_id).is_some())
    })
    .await;
    for service in &all {
        let ad = service.get(&ad_id).unwrap();
        assert_eq!(ad.fiat_currency.as_str(), "KES");
        assert_eq!(ad.status, AdvertisementStatus::Active);
    }

    all[1]
        .set_status(ad_id.clone(), AdvertisementStatus::Disabled)
        .unwrap();
    drive_until(&mut all, |services| {
        services
            .iter()
            .all(|s| s.get(&ad_id).unwrap().status == AdvertisementStatus::Disabled)
    })
    .await;

    // And back. A status that only travelled one way is what this event
    // replaced — every node has to converge on the reactivation too, or a
    // merchant's advertisement comes back for some peers and not others.
    all[1]
        .set_status(ad_id.clone(), AdvertisementStatus::Active)
        .unwrap();
    drive_until(&mut all, |services| {
        services
            .iter()
            .all(|s| s.get(&ad_id).unwrap().status == AdvertisementStatus::Active)
    })
    .await;
}
