//! An attachment published on one node reaches every other node, and a
//! record published by someone who is not a party to the settlement
//! replicates just the same but is never returned to a reader.
//!
//! The second half is the one worth having. Every node stores what it
//! receives — that is what gossip does — so "the stranger's record is
//! rejected" would be the wrong claim to test for. What must hold is that
//! no node *shows* it, on every node independently, with no coordination
//! about which records to hide.

use futures::future::select_all;
use openfiat_content::{AttachmentId, AttachmentService, MediaType};
use openfiat_crypto::{Cid, Keypair};
use openfiat_gossip::EventStore;
use openfiat_gossip::channel::Subscription;
use openfiat_network::identity::{peer_id, to_libp2p_keypair};
use openfiat_network::{Multiaddr, Node};
use openfiat_settlement::SettlementId;
use openfiat_storage::mem::MemoryStore;
use openfiat_types::{PeerId, PublicKey};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

/// The CID this project uploaded to Filebase; see `openfiat_crypto::cid`.
const RECEIPT_CID: &str = "bafkreibdmq27skp3wnycoyoqcei47etyaulerpsegivlkfvyhjkw7ufjva";

fn make_service(seed: u8) -> AttachmentService<MemoryStore> {
    let keypair = Keypair::from_seed([seed; 32]);
    let node = Node::new(&keypair).unwrap();
    let event_store = EventStore::new(MemoryStore::new());
    let gossip =
        openfiat_gossip::GossipService::new(node, event_store, keypair, vec![], Subscription::All);
    AttachmentService::new(gossip, MemoryStore::new())
}

fn identity(seed: u8) -> (PeerId, PublicKey) {
    let keypair = Keypair::from_seed([seed; 32]);
    (peer_id(&to_libp2p_keypair(&keypair)), keypair.public_key())
}

async fn drive_until(
    services: &mut [AttachmentService<MemoryStore>],
    mut condition: impl FnMut(&[AttachmentService<MemoryStore>]) -> bool,
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

async fn listen_addr(service: &mut AttachmentService<MemoryStore>) -> Multiaddr {
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
async fn an_attachment_replicates_and_a_non_partys_is_shown_by_nobody() {
    // 1 = buyer, 2 = seller, 3 = a stranger who is also a node operator.
    let seeds: [u8; 3] = [1, 2, 3];
    let mut all: Vec<AttachmentService<MemoryStore>> =
        seeds.iter().map(|&seed| make_service(seed)).collect();

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

    let settlement = SettlementId::new("settlement-1");
    let cid = Cid::parse(RECEIPT_CID).unwrap();
    let parties = vec![identities[0].0.clone(), identities[1].0.clone()];

    all[0]
        .publish(
            "att-buyer",
            settlement.clone(),
            cid.clone(),
            MediaType::Png,
            31,
            "bank transfer receipt",
        )
        .unwrap();

    // The stranger publishes a well-formed, correctly signed record
    // naming the same settlement.
    all[2]
        .publish(
            "att-stranger",
            settlement.clone(),
            cid,
            MediaType::Png,
            31,
            "definitely genuine evidence",
        )
        .unwrap();

    drive_until(&mut all, |services| {
        services.iter().all(|s| {
            s.get(&AttachmentId::new("att-buyer")).is_some()
                && s.get(&AttachmentId::new("att-stranger")).is_some()
        })
    })
    .await;

    for (index, service) in all.iter().enumerate() {
        let visible = service.find_by_settlement(&settlement, &parties);
        assert_eq!(
            visible.len(),
            1,
            "node {index} showed {} attachments; only the buyer's may be visible",
            visible.len()
        );
        assert_eq!(visible[0].id.as_str(), "att-buyer");
        assert_eq!(visible[0].author, identities[0].0);
        assert_eq!(
            visible[0].cid.as_str(),
            RECEIPT_CID,
            "the CID must survive replication exactly, or the bytes a \
             viewer fetches are not the ones the author signed for"
        );
        assert_eq!(visible[0].caption, "bank transfer receipt");
    }
}
