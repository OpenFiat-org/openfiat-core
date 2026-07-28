//! The Phase 6b exit criterion for identity: a claim published on one
//! node reaches every other node, and a subsequent verify/revoke by the
//! claim's own wallet converges to the same state everywhere.

use futures::future::select_all;
use openfiat_crypto::Keypair;
use openfiat_gossip::EventStore;
use openfiat_gossip::channel::Subscription;
use openfiat_identity::{ClaimType, IdentityService, VerificationStatus};
use openfiat_network::identity::{peer_id, to_libp2p_keypair};
use openfiat_network::{Multiaddr, Node};
use openfiat_storage::mem::MemoryStore;
use openfiat_types::{PeerId, PublicKey};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

fn make_service(seed: u8) -> IdentityService<MemoryStore> {
    let keypair = Keypair::from_seed([seed; 32]);
    let node = Node::new(&keypair).unwrap();
    let event_store = EventStore::new(MemoryStore::new());
    let gossip =
        openfiat_gossip::GossipService::new(node, event_store, keypair, vec![], Subscription::All);
    IdentityService::new(gossip, MemoryStore::new())
}

fn identity(seed: u8) -> (PeerId, PublicKey) {
    let keypair = Keypair::from_seed([seed; 32]);
    (peer_id(&to_libp2p_keypair(&keypair)), keypair.public_key())
}

async fn drive_until(
    services: &mut [IdentityService<MemoryStore>],
    mut condition: impl FnMut(&[IdentityService<MemoryStore>]) -> bool,
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

async fn listen_addr(service: &mut IdentityService<MemoryStore>) -> Multiaddr {
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
async fn a_published_claim_replicates_and_converges_through_verify_and_revoke() {
    let seeds: [u8; 3] = [1, 2, 3];
    let mut all: Vec<IdentityService<MemoryStore>> =
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

    let claim_id = all[0]
        .publish(
            "claim-1",
            ClaimType::Email,
            "merchant@example.com",
            false,
            None,
            None,
        )
        .unwrap();
    drive_until(&mut all, |services| {
        services.iter().all(|s| s.get(&claim_id).is_some())
    })
    .await;
    for service in &all {
        assert_eq!(
            service.get(&claim_id).unwrap().verification_status,
            VerificationStatus::Unverified
        );
    }

    all[0].verify(claim_id.clone()).unwrap();
    drive_until(&mut all, |services| {
        services
            .iter()
            .all(|s| s.get(&claim_id).unwrap().verification_status == VerificationStatus::Verified)
    })
    .await;

    all[0].revoke(claim_id.clone()).unwrap();
    drive_until(&mut all, |services| {
        services.iter().all(|s| s.get(&claim_id).unwrap().revoked)
    })
    .await;

    for service in &all {
        let claim = service.get(&claim_id).unwrap();
        assert!(claim.revoked);
        assert_eq!(service.find_by_wallet(&identities[0].0).len(), 1);
    }
}
