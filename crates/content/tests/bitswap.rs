//! Two real nodes, one real connection, one real block.
//!
//! Everything in `bitswap::message` and `bitswap::serve` is unit-tested
//! against buffers, which proves the encoding and the decision but not
//! that a node can actually serve content. This stands up two libp2p
//! swarms, connects them over TCP, and has one fetch a block from the
//! other by CID — the thing the reward premium is paid for.

use futures::StreamExt;
use libp2p::multiaddr::Protocol;
use libp2p::swarm::SwarmEvent;
use libp2p::{Multiaddr, PeerId};
use openfiat_content::bitswap::message::{Message, Presence, Want, WantType};
use openfiat_content::bitswap::serve::{BlockSource, PROTOCOL, read_all, respond, write_message};
use openfiat_crypto::{Cid, Keypair};
use openfiat_network::Node;
use std::collections::HashMap;
use std::time::Duration;

/// The probe file this project genuinely uploaded to IPFS, and the CID
/// the provider returned for it — the same pair `openfiat_crypto::cid`'s
/// tests check the digest against.
const PROBE_CID: &str = "bafkreibdmq27skp3wnycoyoqcei47etyaulerpsegivlkfvyhjkw7ufjva";
const PROBE_CONTENT: &[u8] = b"openfiat ipfs probe 1785426891\n";
const ABSENT_CID: &str = "bafkreibqyjcrlslvz3uen3qjl6gaqyxu2tryyvqlb555rluyyszpg5zbqu";

struct Blocks(HashMap<String, Vec<u8>>);

impl BlockSource for Blocks {
    fn block(&self, cid: &Cid) -> Option<Vec<u8>> {
        self.0.get(cid.as_str()).cloned()
    }
}

/// Stands up a node listening on a loopback port, returning it with the
/// address it actually bound and a handle for opening streams.
async fn listening_node(seed: [u8; 32]) -> (Node, Multiaddr, PeerId, libp2p_stream::Control) {
    let mut node = Node::new(&Keypair::from_seed(seed)).expect("transport");
    let control = node.content_control();
    let peer = node.libp2p_peer_id();
    node.listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .expect("listen");

    let address = loop {
        if let SwarmEvent::NewListenAddr { address, .. } = node.next_event().await {
            break address;
        }
    };
    (node, address, peer, control)
}

/// The serving half a node runs: accept inbound bitswap streams, answer
/// from what it holds, and send the answer back over a fresh stream.
///
/// This is the loop the RPC actor runs in production, written out here so
/// the test exercises the same sequence rather than a simplification of
/// it — in particular the reply going out on a *new* stream, which is the
/// part of bitswap that would deadlock if it were modelled as a response.
fn serve(mut control: libp2p_stream::Control, blocks: Blocks) {
    tokio::spawn(async move {
        let mut incoming = control.accept(PROTOCOL).expect("accept");
        while let Some((peer, mut stream)) = incoming.next().await {
            for request in read_all(&mut stream).await {
                let reply = respond(&blocks, &request);
                if reply.is_empty() {
                    continue;
                }
                let mut outbound = control
                    .clone()
                    .open_stream(peer, PROTOCOL)
                    .await
                    .expect("open");
                write_message(&mut outbound, &reply).await.expect("write");
            }
        }
    });
}

async fn drive(mut node: Node) {
    loop {
        node.next_event().await;
    }
}

async fn ask(control: &mut libp2p_stream::Control, server: PeerId, want: Want) -> Vec<Message> {
    let mut incoming = control.accept(PROTOCOL).expect("accept");
    let mut outbound = control.open_stream(server, PROTOCOL).await.expect("open");
    write_message(
        &mut outbound,
        &Message {
            wants: vec![want],
            ..Message::default()
        },
    )
    .await
    .expect("write");

    let (_, mut reply) = tokio::time::timeout(Duration::from_secs(10), incoming.next())
        .await
        .expect("the server must answer rather than leaving us to time out")
        .expect("the stream of inbound streams must not end");
    read_all(&mut reply).await
}

#[tokio::test]
async fn a_node_serves_a_block_to_a_peer_that_asks_for_it_by_cid() {
    let cid = Cid::parse(PROBE_CID).unwrap();
    let (server, address, server_peer, server_control) = listening_node([1u8; 32]).await;
    serve(
        server_control,
        Blocks(HashMap::from([(
            PROBE_CID.to_string(),
            PROBE_CONTENT.to_vec(),
        )])),
    );
    tokio::spawn(drive(server));

    let mut client = Node::new(&Keypair::from_seed([2u8; 32])).expect("transport");
    let mut client_control = client.content_control();
    client
        .dial(address.with(Protocol::P2p(server_peer)))
        .expect("dial");
    tokio::spawn(drive(client));

    let messages = ask(
        &mut client_control,
        server_peer,
        Want {
            cid: cid.clone(),
            want_type: WantType::Block,
            cancel: false,
            send_dont_have: true,
        },
    )
    .await;

    let blocks: Vec<_> = messages.iter().flat_map(|m| m.blocks.clone()).collect();
    assert_eq!(blocks, vec![(cid.clone(), PROBE_CONTENT.to_vec())]);
    // The identifier was rebuilt from the bytes that arrived, so this is
    // not merely "the server said so" — it is the content the CID names.
    assert!(cid.matches(&blocks[0].1));
}

#[tokio::test]
async fn a_peer_asking_for_content_this_node_lacks_is_told_so_rather_than_left_waiting() {
    let absent = Cid::parse(ABSENT_CID).unwrap();
    let (server, address, server_peer, server_control) = listening_node([3u8; 32]).await;
    serve(
        server_control,
        Blocks(HashMap::from([(
            PROBE_CID.to_string(),
            PROBE_CONTENT.to_vec(),
        )])),
    );
    tokio::spawn(drive(server));

    let mut client = Node::new(&Keypair::from_seed([4u8; 32])).expect("transport");
    let mut client_control = client.content_control();
    client
        .dial(address.with(Protocol::P2p(server_peer)))
        .expect("dial");
    tokio::spawn(drive(client));

    let messages = ask(
        &mut client_control,
        server_peer,
        Want {
            cid: absent.clone(),
            want_type: WantType::Block,
            cancel: false,
            send_dont_have: true,
        },
    )
    .await;

    let presences: Vec<_> = messages.iter().flat_map(|m| m.presences.clone()).collect();
    assert_eq!(presences, vec![(absent, Presence::DontHave)]);
    assert!(
        messages.iter().all(|m| m.blocks.is_empty()),
        "a node that does not hold content must not send any"
    );
}
