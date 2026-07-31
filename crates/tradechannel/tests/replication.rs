//! What a trade channel looks like on a real gossip cluster.
//!
//! The point of this test is not that the events arrive — every domain in
//! this workspace has a test for that. It is what arrives: a third node
//! that is not a party to the trade replicates the payment details and
//! the whole conversation, stores them, serves them, and cannot read one
//! byte of either. That is the claim the word "sealed" has to survive,
//! and a single-process unit test cannot make it, because the thing under
//! test is what the *network* carries.

use futures::future::select_all;
use openfiat_crypto::Keypair;
use openfiat_disputes::events::{
    ArbitratorJoin, DisputeOpen, SignedArbitratorJoin, SignedDisputeOpen,
};
use openfiat_disputes::{DisputeId, DisputeRegistry};
use openfiat_gossip::EventStore;
use openfiat_gossip::channel::Subscription;
use openfiat_network::identity::{peer_id, peer_id_from_public_key, to_libp2p_keypair};
use openfiat_network::{Multiaddr, Node};
use openfiat_reservations::ReservationId;
use openfiat_settlement::events::{SettlementInitiate, SignedSettlementInitiate};
use openfiat_settlement::{SettlementId, SettlementRegistry};
use openfiat_storage::mem::MemoryStore;
use openfiat_tradechannel::{ChannelKey, EntryKind, TradeChannelService, open_entry};
use openfiat_types::{Amount, PeerId, PublicKey, Timestamp};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::time::Duration;

const BUYER_SEED: u8 = 1;
const SELLER_SEED: u8 = 2;
const OUTSIDER_SEED: u8 = 3;
const ARBITRATOR_SEED: u8 = 4;

const ACCOUNT_NUMBER: &[u8] = b"Equity Bank 0110123456789, R. Kimani";

fn keypair(seed: u8) -> Keypair {
    Keypair::from_seed([seed; 32])
}

fn identity(seed: u8) -> (PeerId, PublicKey) {
    let keypair = keypair(seed);
    (peer_id(&to_libp2p_keypair(&keypair)), keypair.public_key())
}

/// Every node in a real cluster has already replicated the settlement
/// before anything can be written to its channel, so each one gets its
/// own registry seeded with the same signed event.
fn seeded_settlements(settlement_id: &SettlementId) -> Rc<SettlementRegistry<MemoryStore>> {
    let buyer = keypair(BUYER_SEED);
    let seller = keypair(SELLER_SEED);
    let registry = Rc::new(SettlementRegistry::new(MemoryStore::new()));
    registry
        .apply_initiate(SignedSettlementInitiate::sign(
            SettlementInitiate {
                id: settlement_id.clone(),
                reservation_id: ReservationId::new("res-1"),
                buyer: peer_id_from_public_key(&buyer.public_key()).unwrap(),
                buyer_public_key: buyer.public_key(),
                seller: peer_id_from_public_key(&seller.public_key()).unwrap(),
                seller_public_key: seller.public_key(),
                amount: Amount::new(2_000_000, 6),
                timestamp: Timestamp::now(),
            },
            &buyer,
        ))
        .unwrap();
    registry
}

/// A dispute that the arbitrator has already joined, replicated onto
/// every node the same way. The grant check reads this, so it has to be
/// real state rather than an assumption.
fn seeded_disputes(
    settlement_id: &SettlementId,
    settlements: Rc<SettlementRegistry<MemoryStore>>,
) -> Rc<DisputeRegistry<MemoryStore>> {
    let buyer = keypair(BUYER_SEED);
    let arbitrator = keypair(ARBITRATOR_SEED);
    let registry = Rc::new(DisputeRegistry::new(MemoryStore::new(), settlements));
    registry
        .apply_open(SignedDisputeOpen::sign(
            DisputeOpen {
                id: DisputeId::new("dispute-1"),
                settlement_id: settlement_id.clone(),
                opener: peer_id_from_public_key(&buyer.public_key()).unwrap(),
                opener_public_key: buyer.public_key(),
                reason: "the account they gave me was closed".to_string(),
                timestamp: Timestamp::now(),
            },
            &buyer,
        ))
        .unwrap();
    registry
        .apply_arbitrator_join(SignedArbitratorJoin::sign(
            ArbitratorJoin {
                dispute_id: DisputeId::new("dispute-1"),
                arbitrator: peer_id_from_public_key(&arbitrator.public_key()).unwrap(),
                arbitrator_public_key: arbitrator.public_key(),
                timestamp: Timestamp::now(),
            },
            &arbitrator,
        ))
        .unwrap();
    registry
}

fn make_service(seed: u8, settlement_id: &SettlementId) -> TradeChannelService<MemoryStore> {
    let keypair = keypair(seed);
    let node = Node::new(&keypair).unwrap();
    let event_store = EventStore::new(MemoryStore::new());
    let gossip =
        openfiat_gossip::GossipService::new(node, event_store, keypair, vec![], Subscription::All);
    let settlements = seeded_settlements(settlement_id);
    let disputes = seeded_disputes(settlement_id, Rc::clone(&settlements));
    TradeChannelService::new(gossip, MemoryStore::new(), settlements, disputes)
}

async fn drive_until(
    services: &mut [TradeChannelService<MemoryStore>],
    mut condition: impl FnMut(&[TradeChannelService<MemoryStore>]) -> bool,
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

async fn listen_addr(service: &mut TradeChannelService<MemoryStore>) -> Multiaddr {
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
async fn payment_details_and_chat_replicate_to_every_node_and_open_for_nobody_but_the_parties() {
    let settlement_id = SettlementId::new("settle-1");
    let seeds = [BUYER_SEED, SELLER_SEED, OUTSIDER_SEED];
    let mut all: Vec<TradeChannelService<MemoryStore>> = seeds
        .iter()
        .map(|&seed| make_service(seed, &settlement_id))
        .collect();

    let hub_addr = listen_addr(&mut all[0]).await;
    let identities: Vec<_> = seeds.iter().map(|&seed| identity(seed)).collect();
    for (i, service) in all.iter_mut().enumerate() {
        for (j, (peer, public_key)) in identities.iter().enumerate() {
            if i != j {
                service.gossip.register_peer_key(peer.clone(), *public_key);
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

    // The seller opens the channel: one key, sealed to the buyer, and a
    // second grant to themselves so their own client can recover it.
    let key = ChannelKey::generate();
    let (buyer_id, buyer_key) = identity(BUYER_SEED);
    let (seller_id, seller_key) = identity(SELLER_SEED);
    all[1]
        .grant_key(settlement_id.clone(), buyer_id.clone(), &buyer_key, &key)
        .unwrap();
    all[1]
        .grant_key(settlement_id.clone(), seller_id.clone(), &seller_key, &key)
        .unwrap();
    all[1]
        .post(
            settlement_id.clone(),
            EntryKind::PaymentDetails,
            &key,
            ACCOUNT_NUMBER,
        )
        .unwrap();
    all[0]
        .post(
            settlement_id.clone(),
            EntryKind::Message,
            &key,
            b"sent, reference 88213",
        )
        .unwrap();

    drive_until(&mut all, |services| {
        services.iter().all(|s| {
            let channel = s.channel(&settlement_id);
            channel.grants.len() == 2 && channel.entries.len() == 2
        })
    })
    .await;

    // Every node — including the one that is not in this trade — holds
    // the same channel.
    for service in &all {
        let channel = service.channel(&settlement_id);
        assert_eq!(channel.payment_details().len(), 1);
        assert_eq!(channel.messages().len(), 1);
        assert_eq!(channel.readers(), {
            let mut expected = vec![buyer_id.clone(), seller_id.clone()];
            expected.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
            expected
        });
    }

    // The buyer recovers the key from their grant and reads both entries.
    let outsider_view = all[2].channel(&settlement_id);
    let buyer_grant = outsider_view.grants_for(&buyer_id)[0];
    let recovered = ChannelKey::from_bytes(
        openfiat_crypto::open(&keypair(BUYER_SEED), &buyer_grant.sealed_key)
            .expect("the grant is sealed to the buyer")
            .try_into()
            .unwrap(),
    );
    let opened: Vec<Vec<u8>> = outsider_view
        .entries
        .iter()
        .map(|entry| open_entry(&recovered, &entry.binding(), &entry.payload).unwrap())
        .collect();
    assert!(opened.contains(&ACCOUNT_NUMBER.to_vec()));
    assert!(opened.contains(&b"sent, reference 88213".to_vec()));

    // The third node replicated all of it and can read none of it. This
    // is the assertion the feature stands or falls on: gossip reaches
    // everyone, and confidentiality does not depend on who is asking.
    let outsider = keypair(OUTSIDER_SEED);
    for grant in &outsider_view.grants {
        assert!(
            openfiat_crypto::open(&outsider, &grant.sealed_key).is_err(),
            "a node that is not a party must not be able to unseal the key"
        );
    }
    for entry in &outsider_view.entries {
        assert!(
            !entry
                .payload
                .ciphertext
                .windows(ACCOUNT_NUMBER.len())
                .any(|window| window == ACCOUNT_NUMBER),
            "the replicated bytes must not contain the account number"
        );
        assert!(
            open_entry(&ChannelKey::generate(), &entry.binding(), &entry.payload).is_err(),
            "without a grant there is no key, and without the key there is \
             nothing but ciphertext"
        );
    }
}

#[tokio::test]
async fn a_party_discloses_the_channel_to_an_arbitrator_after_the_dispute_opens() {
    let settlement_id = SettlementId::new("settle-1");
    let seeds = [SELLER_SEED, ARBITRATOR_SEED];
    let mut all: Vec<TradeChannelService<MemoryStore>> = seeds
        .iter()
        .map(|&seed| make_service(seed, &settlement_id))
        .collect();

    let hub_addr = listen_addr(&mut all[0]).await;
    let identities: Vec<_> = seeds.iter().map(|&seed| identity(seed)).collect();
    for (i, service) in all.iter_mut().enumerate() {
        for (j, (peer, public_key)) in identities.iter().enumerate() {
            if i != j {
                service.gossip.register_peer_key(peer.clone(), *public_key);
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

    // The conversation happens first, with no arbitrator in existence as
    // far as the parties are concerned.
    let key = ChannelKey::generate();
    let (buyer_id, buyer_key) = identity(BUYER_SEED);
    all[0]
        .grant_key(settlement_id.clone(), buyer_id, &buyer_key, &key)
        .unwrap();
    all[0]
        .post(
            settlement_id.clone(),
            EntryKind::PaymentDetails,
            &key,
            ACCOUNT_NUMBER,
        )
        .unwrap();
    drive_until(&mut all, |services| {
        services
            .iter()
            .all(|s| s.channel(&settlement_id).entries.len() == 1)
    })
    .await;

    // The arbitrator's own node holds the ciphertext and cannot read it,
    // even though they have joined the case.
    let arbitrator = keypair(ARBITRATOR_SEED);
    let (arbitrator_id, arbitrator_key) = identity(ARBITRATOR_SEED);
    let before = all[1].channel(&settlement_id);
    assert!(
        !before.is_reader(&arbitrator_id),
        "joining a dispute does not by itself open the conversation"
    );

    // The seller discloses. One 32-byte grant, no re-encryption.
    all[0]
        .grant_key(
            settlement_id.clone(),
            arbitrator_id.clone(),
            &arbitrator_key,
            &key,
        )
        .unwrap();
    drive_until(&mut all, |services| {
        services
            .iter()
            .all(|s| s.channel(&settlement_id).is_reader(&arbitrator_id))
    })
    .await;

    let after = all[1].channel(&settlement_id);
    assert_eq!(
        after.entries, before.entries,
        "disclosure must not rewrite a single stored entry — the \
         arbitrator reads the ciphertexts the network carried at the time"
    );
    let grant = after.grants_for(&arbitrator_id)[0];
    let recovered = ChannelKey::from_bytes(
        openfiat_crypto::open(&arbitrator, &grant.sealed_key)
            .expect("the grant is sealed to the arbitrator")
            .try_into()
            .unwrap(),
    );
    let entry = &after.entries[0];
    assert_eq!(
        open_entry(&recovered, &entry.binding(), &entry.payload).unwrap(),
        ACCOUNT_NUMBER
    );
}
