//! OGP §17/§26 ("support eventual consistency") and OFNP's connection
//! lifecycle proved at the *composed* node level: a node running every
//! domain drops offline mid-cluster, misses events across two unrelated
//! domains (governance, identity) at once, then reconnects and recovers
//! both — not just the one domain `gossip/tests/propagation.rs` already
//! proves this for in isolation.

use openfiat_conformance::{FullNode, identity};
use openfiat_crypto::Keypair;
use openfiat_governance::events::{ProposalCreate, SignedProposalCreate};
use openfiat_governance::record::ProposalCategory;
use openfiat_governance::{ProposalId, protocol as gov_protocol};
use openfiat_identity::events::{ClaimPublish, SignedClaimPublish};
use openfiat_identity::record::ClaimType;
use openfiat_identity::{ClaimId, protocol as id_protocol};
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_network::{Multiaddr, Node};
use openfiat_storage::mem::MemoryStore;
use openfiat_types::{NodeRole, Priority, Timestamp};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

fn make_node(seed: u8) -> FullNode<MemoryStore> {
    let node = Node::new(&Keypair::from_seed([seed; 32])).unwrap();
    FullNode::new(
        node,
        MemoryStore::new(),
        Keypair::from_seed([seed; 32]),
        vec![NodeRole::FullNode],
    )
}

async fn listen_addr(node: &mut FullNode<MemoryStore>) -> Multiaddr {
    node.gossip
        .node
        .listen_on("/ip4/127.0.0.1/udp/0/quic-v1".parse().unwrap())
        .unwrap();
    loop {
        if let libp2p::swarm::SwarmEvent::NewListenAddr { address, .. } =
            node.gossip.node.next_event().await
        {
            return address;
        }
    }
}

async fn drive_until(
    nodes: &mut [FullNode<MemoryStore>],
    mut condition: impl FnMut(&[FullNode<MemoryStore>]) -> bool,
) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !condition(nodes) {
            let futures: Vec<Pin<Box<dyn Future<Output = ()> + '_>>> = nodes
                .iter_mut()
                .map(|n| Box::pin(n.drive_once()) as Pin<Box<dyn Future<Output = ()> + '_>>)
                .collect();
            futures::future::select_all(futures).await;
        }
    })
    .await
    .expect("did not reach the expected state within 15 seconds")
}

async fn drive_briefly(nodes: &mut [FullNode<MemoryStore>], duration: Duration) {
    let _ = tokio::time::timeout(duration, async {
        loop {
            let futures: Vec<Pin<Box<dyn Future<Output = ()> + '_>>> = nodes
                .iter_mut()
                .map(|n| Box::pin(n.drive_once()) as Pin<Box<dyn Future<Output = ()> + '_>>)
                .collect();
            futures::future::select_all(futures).await;
        }
    })
    .await;
}

#[tokio::test]
async fn an_offline_node_recovers_events_from_two_domains_at_once_on_reconnect() {
    let mut a = make_node(30); // author / publisher, stays online
    let mut b = make_node(31); // drops offline

    let a_addr = listen_addr(&mut a).await;
    let (a_id, a_key) = identity(&Keypair::from_seed([30u8; 32]));
    let (b_id, b_key) = identity(&Keypair::from_seed([31u8; 32]));
    a.gossip.register_peer_key(b_id, b_key);
    b.gossip.register_peer_key(a_id, a_key);
    b.gossip.node.dial(a_addr.clone()).unwrap();

    let mut all = vec![a, b];
    drive_until(&mut all, |nodes| {
        nodes.iter().all(|n| n.gossip.connected_peer_count() >= 1)
    })
    .await;

    all[1].gossip.disconnect_all();
    drive_until(&mut all, |nodes| {
        nodes.iter().all(|n| n.gossip.connected_peer_count() == 0)
    })
    .await;

    let author = Keypair::from_seed([30u8; 32]);
    let author_peer = peer_id_from_public_key(&author.public_key()).unwrap();

    let proposal_id = ProposalId::new("ofp-partition-1");
    let proposal = ProposalCreate {
        id: proposal_id.clone(),
        title: "Test proposal published while a peer is offline".to_string(),
        summary: "Exercises OGP eventual consistency across two domains.".to_string(),
        category: ProposalCategory::Protocol,
        author: author_peer.clone(),
        author_public_key: author.public_key(),
        // Gossip convergence across a partition, not chain agreement:
        // this proposal has no on-chain counterpart to claim.
        onchain_proposal_id: None,
        timestamp: Timestamp::now(),
    };
    let signed_proposal = SignedProposalCreate::sign(proposal, &author);
    all[0]
        .originate(
            gov_protocol::EVENT_CREATED,
            gov_protocol::OFS_SPEC,
            Priority::Governance,
            8,
            &signed_proposal,
        )
        .unwrap();

    let claim_id = ClaimId::new("claim-partition-1");
    let claim = ClaimPublish {
        id: claim_id.clone(),
        wallet: author_peer.clone(),
        wallet_public_key: author.public_key(),
        claim_type: ClaimType::Email,
        value: "offline-test@example.com".to_string(),
        verified: false,
        supersedes: None,
        expires_at: None,
        timestamp: Timestamp::now(),
    };
    let signed_claim = SignedClaimPublish::sign(claim, &author);
    all[0]
        .originate(
            id_protocol::EVENT_CREATED,
            id_protocol::OFS_SPEC,
            Priority::Reputation,
            8,
            &signed_claim,
        )
        .unwrap();

    drive_briefly(&mut all, Duration::from_millis(200)).await;
    assert!(
        all[1].governance.get(&proposal_id).is_none(),
        "an offline node must not receive governance events sent while disconnected"
    );
    assert!(
        all[1].identity.get(&claim_id).is_none(),
        "an offline node must not receive identity events sent while disconnected"
    );

    all[1].gossip.node.dial(a_addr).unwrap();
    drive_until(&mut all, |nodes| {
        nodes[1].governance.get(&proposal_id).is_some()
            && nodes[1].identity.get(&claim_id).is_some()
    })
    .await;

    assert!(
        all[1].governance.get(&proposal_id).is_some(),
        "reconnecting must recover the governance proposal missed while offline"
    );
    assert!(
        all[1].identity.get(&claim_id).is_some(),
        "reconnecting must recover the identity claim missed while offline"
    );
}
