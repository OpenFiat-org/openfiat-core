//! A session established, renewed, and migrated to a new host converges
//! identically across every node in the cluster — the same replication
//! proof pattern used by every other crate in this workspace.

use futures::future::select_all;
use openfiat_crypto::Keypair;
use openfiat_gossip::EventStore;
use openfiat_gossip::channel::Subscription;
use openfiat_network::identity::{peer_id, to_libp2p_keypair};
use openfiat_network::{Multiaddr, Node};
use openfiat_sessions::SessionService;
use openfiat_storage::mem::MemoryStore;
use openfiat_types::{PeerId, PublicKey};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

fn make_service(seed: u8) -> SessionService<MemoryStore> {
    let keypair = Keypair::from_seed([seed; 32]);
    let node = Node::new(&keypair).unwrap();
    let event_store = EventStore::new(MemoryStore::new());
    let gossip =
        openfiat_gossip::GossipService::new(node, event_store, keypair, vec![], Subscription::All);
    SessionService::new(gossip, MemoryStore::new())
}

fn identity(seed: u8) -> (PeerId, PublicKey) {
    let keypair = Keypair::from_seed([seed; 32]);
    (peer_id(&to_libp2p_keypair(&keypair)), keypair.public_key())
}

async fn drive_until(
    services: &mut [SessionService<MemoryStore>],
    mut condition: impl FnMut(&[SessionService<MemoryStore>]) -> bool,
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

async fn listen_addr(service: &mut SessionService<MemoryStore>) -> Multiaddr {
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
async fn a_session_establishes_renews_and_migrates_across_the_cluster() {
    let seeds: [u8; 3] = [1, 2, 3];
    let mut all: Vec<SessionService<MemoryStore>> =
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

    let session_id = all[0]
        .establish("sess-1", "web", vec!["trade".to_string()], None)
        .unwrap();
    drive_until(&mut all, |services| {
        services.iter().all(|s| s.get(&session_id).is_some())
    })
    .await;
    for service in &all {
        assert!(
            service
                .get(&session_id)
                .unwrap()
                .is_current(openfiat_types::Timestamp::now())
        );
    }

    all[0].renew(session_id.clone(), 1, None).unwrap();
    drive_until(&mut all, |services| {
        services
            .iter()
            .all(|s| s.get(&session_id).unwrap().version == 1)
    })
    .await;

    let new_host = identities[2].0.clone();
    all[0]
        .migrate(session_id.clone(), new_host.clone(), 2)
        .unwrap();
    drive_until(&mut all, |services| {
        services
            .iter()
            .all(|s| s.get(&session_id).unwrap().host_node == new_host)
    })
    .await;

    for service in &all {
        let session = service.get(&session_id).unwrap();
        assert_eq!(session.host_node, new_host);
        assert_eq!(session.version, 2);
        assert!(session.is_current(openfiat_types::Timestamp::now()));
        assert_eq!(service.find_by_wallet(&identities[0].0).len(), 1);
    }
}
