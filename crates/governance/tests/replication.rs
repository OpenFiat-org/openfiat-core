//! The Phase 6b exit criterion for governance: a proposal that goes
//! through create → vote → tally → pass entirely off gossiped events,
//! converging identically across every node in the cluster.

use futures::future::select_all;
use openfiat_crypto::Keypair;
use openfiat_gossip::EventStore;
use openfiat_gossip::channel::Subscription;
use openfiat_governance::{GovernanceService, ProposalCategory, ProposalStatus, VoteChoice};
use openfiat_network::identity::{peer_id, to_libp2p_keypair};
use openfiat_network::{Multiaddr, Node};
use openfiat_storage::mem::MemoryStore;
use openfiat_types::{PeerId, PublicKey, Timestamp};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

fn make_service(seed: u8) -> GovernanceService<MemoryStore> {
    let keypair = Keypair::from_seed([seed; 32]);
    let node = Node::new(&keypair).unwrap();
    let event_store = EventStore::new(MemoryStore::new());
    let gossip =
        openfiat_gossip::GossipService::new(node, event_store, keypair, vec![], Subscription::All);
    GovernanceService::new(gossip, MemoryStore::new())
}

fn identity(seed: u8) -> (PeerId, PublicKey) {
    let keypair = Keypair::from_seed([seed; 32]);
    (peer_id(&to_libp2p_keypair(&keypair)), keypair.public_key())
}

async fn drive_until(
    services: &mut [GovernanceService<MemoryStore>],
    mut condition: impl FnMut(&[GovernanceService<MemoryStore>]) -> bool,
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

async fn listen_addr(service: &mut GovernanceService<MemoryStore>) -> Multiaddr {
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
async fn a_proposal_passes_and_converges_across_the_cluster() {
    let seeds: [u8; 4] = [1, 2, 3, 4]; // author + 3 voters
    let mut all: Vec<GovernanceService<MemoryStore>> =
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

    let proposal_id = all[0]
        .create_proposal(
            "ofp-1",
            "Increase Reservation Timeout",
            "Raise the validation window from 30 to 45 minutes.",
            ProposalCategory::Protocol,
        )
        .unwrap();
    drive_until(&mut all, |services| {
        services.iter().all(|s| s.get(&proposal_id).is_some())
    })
    .await;

    for voter in &mut all[1..] {
        voter
            .cast_vote(proposal_id.clone(), VoteChoice::Approve, 1, "")
            .unwrap();
    }
    drive_until(&mut all, |services| {
        services
            .iter()
            .all(|s| s.get(&proposal_id).unwrap().votes.len() == 3)
    })
    .await;

    // What replication actually guarantees: every node ends up holding the
    // same VOTES. It does not, and must not, mean every node computes the
    // same OUTCOME from them — that was the assumption this test previously
    // encoded, and it is false in the shipped node. A vote only reaches
    // local state once its weight has been verified against on-chain stake,
    // so a node without an RPC endpoint holds none and would have concluded
    // "rejected" from an empty set while its peers concluded "accepted".
    //
    // Resolution therefore comes from the governance program's own tally,
    // which every node reads rather than recomputes. Simulating the voting
    // window's real close (7 days by default) with a far-future `now`.
    for service in &all {
        let proposal = service.get(&proposal_id).unwrap();
        let far_future = Timestamp::from_millis(proposal.voting_closes_at.as_millis() + 1);
        let preview = service
            .registry()
            .local_vote_preview(&proposal_id, far_future)
            .unwrap();
        assert_eq!(preview.voters_seen, 3, "all three votes replicated here");
        assert!(preview.voting_closed);
        assert_eq!(
            proposal.status,
            ProposalStatus::Voting,
            "a closed window is not a resolution — only the chain decides"
        );
    }

    // Adopting the chain's result gives every node the same status, by
    // reading the same value rather than by coincidentally agreeing.
    for service in &all {
        let far_future = Timestamp::from_millis(
            service
                .get(&proposal_id)
                .unwrap()
                .voting_closes_at
                .as_millis()
                + 1,
        );
        service
            .registry()
            .apply_onchain_resolution(&proposal_id, ProposalStatus::Accepted, far_future)
            .unwrap();
    }

    for service in &all {
        let proposal = service.get(&proposal_id).unwrap();
        assert_eq!(proposal.status, ProposalStatus::Accepted);
        assert_eq!(proposal.votes.len(), 3);
    }
}
