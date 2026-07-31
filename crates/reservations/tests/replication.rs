//! A reservation request made on one node reaches another node via
//! gossip and is independently validated + applied against that node's
//! own (already-synchronized) advertisement registry — proving
//! `openfiat-reservations` genuinely works across the network, not just
//! against a local index.
//!
//! Advertisement replication itself is already proven by
//! `openfiat-advertisements`'s own integration test, so both nodes here
//! are seeded with the identical advertisement directly rather than
//! standing up a second live gossip network for it.

use futures::future::select_all;
use openfiat_advertisements::events::{AdvertisementCreate, SignedAdvertisementCreate};
use openfiat_advertisements::{AdvertisementId, AdvertisementRegistry, Direction, PricingModel};
use openfiat_crypto::{Keypair, MintAddress};
use openfiat_gossip::EventStore;
use openfiat_gossip::channel::Subscription;
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_network::identity::{peer_id, to_libp2p_keypair};
use openfiat_network::{Multiaddr, Node};
use openfiat_reservations::{ReservationService, ReservationState};
use openfiat_storage::mem::MemoryStore;
use openfiat_types::{Amount, FiatCurrency, PeerId, PublicKey, Timestamp};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::time::Duration;

fn seeded_ad_registry(
    ad_id: &AdvertisementId,
    merchant: &Keypair,
) -> Rc<AdvertisementRegistry<MemoryStore>> {
    let registry = Rc::new(AdvertisementRegistry::new(MemoryStore::new()));
    let create = AdvertisementCreate {
        id: ad_id.clone(),
        merchant: peer_id_from_public_key(&merchant.public_key()).unwrap(),
        merchant_public_key: merchant.public_key(),
        asset_mint: MintAddress::parse("2bHPi5hA4zrmPAfrvLmEexg3KJjpTjNkUcxWnzUPeRRU").unwrap(),
        direction: Direction::Sell,
        fiat_currency: FiatCurrency::parse("KES").unwrap(),
        min_trade: Amount::new(1_000_000, 6),
        max_trade: Amount::new(5_000_000, 6),
        initial_liquidity: Amount::new(10_000_000, 6),
        pricing: PricingModel::Fixed {
            price: Amount::new(129_000_000, 6),
        },
        payment_methods: vec!["Mobile Money".to_string()],
        timestamp: Timestamp::now(),
    };
    registry
        .apply_create(SignedAdvertisementCreate::sign(create, merchant))
        .unwrap();
    registry
}

fn make_service(
    seed: u8,
    ad_registry: Rc<AdvertisementRegistry<MemoryStore>>,
) -> ReservationService<MemoryStore> {
    let keypair = Keypair::from_seed([seed; 32]);
    let node = Node::new(&keypair).unwrap();
    let event_store = EventStore::new(MemoryStore::new());
    let gossip =
        openfiat_gossip::GossipService::new(node, event_store, keypair, vec![], Subscription::All);
    ReservationService::new(gossip, MemoryStore::new(), ad_registry)
}

fn identity(seed: u8) -> (PeerId, PublicKey) {
    let keypair = Keypair::from_seed([seed; 32]);
    (peer_id(&to_libp2p_keypair(&keypair)), keypair.public_key())
}

async fn drive_until(
    services: &mut [ReservationService<MemoryStore>],
    mut condition: impl FnMut(&[ReservationService<MemoryStore>]) -> bool,
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

async fn listen_addr(service: &mut ReservationService<MemoryStore>) -> Multiaddr {
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
async fn a_reservation_request_replicates_and_locks_liquidity_on_every_node() {
    let ad_id = AdvertisementId::new("ad-1");
    let merchant = Keypair::generate();
    let ad_registry_a = seeded_ad_registry(&ad_id, &merchant);
    let ad_registry_b = seeded_ad_registry(&ad_id, &merchant);

    let mut a = make_service(1, ad_registry_a.clone());
    let mut b = make_service(2, ad_registry_b.clone());

    let a_addr = listen_addr(&mut a).await;
    let (a_id, a_key) = identity(1);
    let (b_id, b_key) = identity(2);
    a.gossip.register_peer_key(b_id, b_key);
    b.gossip.register_peer_key(a_id, a_key);
    b.gossip.node.dial(a_addr).unwrap();

    let mut all = vec![a, b];
    drive_until(&mut all, |services| {
        services
            .iter()
            .all(|s| s.gossip.connected_peer_count() >= 1)
    })
    .await;

    let reservation_id = all[0]
        .request(
            "res-1",
            ad_id.clone(),
            Amount::new(2_000_000, 6),
            // The fixed price this advertisement was published at. A
            // reservation carrying anything else is refused — see
            // `PricingModel::agrees_with`.
            Amount::new(129_000_000, 6),
            None,
        )
        .unwrap();

    drive_until(&mut all, |services| {
        services.iter().all(|s| s.get(&reservation_id).is_some())
    })
    .await;

    for service in &all {
        let reservation = service.get(&reservation_id).unwrap();
        assert_eq!(reservation.state, ReservationState::EscrowLocked);
        assert_eq!(reservation.amount, Amount::new(2_000_000, 6));
    }
    assert_eq!(
        ad_registry_a.get(&ad_id).unwrap().available_liquidity,
        Amount::new(8_000_000, 6)
    );
    assert_eq!(
        ad_registry_b.get(&ad_id).unwrap().available_liquidity,
        Amount::new(8_000_000, 6)
    );

    // Cancel from the same node and confirm it replicates too.
    all[0].cancel(reservation_id.clone()).unwrap();
    drive_until(&mut all, |services| {
        services
            .iter()
            .all(|s| s.get(&reservation_id).unwrap().state == ReservationState::Cancelled)
    })
    .await;

    assert_eq!(
        ad_registry_a.get(&ad_id).unwrap().available_liquidity,
        Amount::new(10_000_000, 6)
    );
    assert_eq!(
        ad_registry_b.get(&ad_id).unwrap().available_liquidity,
        Amount::new(10_000_000, 6)
    );
}
