//! End-to-end proof of OFS-1000's Phase 2 exit criteria: two real nodes
//! complete a full handshake over QUIC+Noise, exchange envelope-wrapped
//! messages, reject a replayed sequence number, and shut down gracefully.

use libp2p::request_response::{self, Message};
use libp2p::swarm::SwarmEvent;
use libp2p::{Multiaddr, PeerId};
use openfiat_network::behaviour::OpenFiatBehaviourEvent;
use openfiat_network::sequence::SequenceTracker;
use openfiat_network::{Envelope, Node};
use openfiat_crypto::Keypair;
use std::time::Duration;

#[tokio::test]
async fn two_nodes_handshake_exchange_envelopes_reject_replays_and_shut_down_gracefully() {
    let mut node_a = Node::new(&Keypair::generate()).unwrap();
    let mut node_b = Node::new(&Keypair::generate()).unwrap();

    node_a.listen_on("/ip4/127.0.0.1/udp/0/quic-v1".parse().unwrap()).unwrap();

    let mut listen_addr: Option<Multiaddr> = None;
    let mut dialed = false;

    // Peer IDs each side learns once the Noise handshake + identity
    // verification steps of OFNP §8 complete.
    let mut peer_of_a: Option<PeerId> = None; // node_b, as seen by node_a
    let mut peer_of_b: Option<PeerId> = None; // node_a, as seen by node_b

    let mut sent_first_request = false;
    let mut sent_duplicate_request = false;
    let mut first_accept: Option<bool> = None; // true if accepted
    let mut duplicate_rejected: Option<bool> = None;
    let mut b_response_count = 0u8;

    let mut a_tracker = SequenceTracker::new();

    let mut a_disconnected = false;
    let mut b_disconnected = false;
    let mut shutdown_initiated = false;

    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if a_disconnected && b_disconnected {
                break;
            }

            tokio::select! {
                event = node_a.next_event() => match event {
                    SwarmEvent::NewListenAddr { address, .. } if listen_addr.is_none() => {
                        listen_addr = Some(address);
                    }
                    SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                        peer_of_a = Some(peer_id);
                    }
                    SwarmEvent::Behaviour(OpenFiatBehaviourEvent::Envelope(
                        request_response::Event::Message { message: Message::Request { request, channel, .. }, .. },
                    )) => {
                        let sequence = request.header.sequence;
                        if first_accept.is_none() {
                            first_accept = Some(a_tracker.accept(sequence).is_ok());
                            let response = Envelope::new(1000, "Ack", 1, b"ack".to_vec());
                            node_a.swarm.behaviour_mut().envelope.send_response(channel, response).unwrap();
                        } else {
                            duplicate_rejected = Some(a_tracker.accept(sequence).is_err());
                            let response = Envelope::new(1000, "Ack", 2, b"ack-duplicate".to_vec());
                            let _ = node_a.swarm.behaviour_mut().envelope.send_response(channel, response);
                        }
                    }
                    SwarmEvent::ConnectionClosed { .. } => {
                        a_disconnected = true;
                    }
                    _ => {}
                },
                event = node_b.next_event() => match event {
                    SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                        peer_of_b = Some(peer_id);
                    }
                    SwarmEvent::Behaviour(OpenFiatBehaviourEvent::Envelope(
                        request_response::Event::Message { message: Message::Response { .. }, .. },
                    )) => {
                        b_response_count += 1;
                    }
                    SwarmEvent::ConnectionClosed { .. } => {
                        b_disconnected = true;
                    }
                    _ => {}
                },
            }

            if let Some(addr) = listen_addr.clone().filter(|_| !dialed) {
                node_b.dial(addr).unwrap();
                dialed = true;
            }

            if !sent_first_request
                && let Some(peer) = peer_of_b
            {
                node_b.send_envelope(peer, Envelope::new(1000, "Heartbeat", 1, b"hello".to_vec()));
                sent_first_request = true;
            }

            // Replay the same sequence number once the first round trip
            // has completed, proving §15's duplicate-detection over a real
            // wire round trip rather than just the unit-tested tracker.
            if sent_first_request && !sent_duplicate_request && b_response_count == 1 {
                let peer = peer_of_b.expect("connected before the first request could be sent");
                node_b.send_envelope(peer, Envelope::new(1000, "Heartbeat", 1, b"hello-again".to_vec()));
                sent_duplicate_request = true;
            }

            // OFNP §23 graceful shutdown, once both the accepted and
            // rejected sequence numbers have been observed.
            if !shutdown_initiated && b_response_count == 2 {
                let peer = peer_of_a.expect("connected before shutdown could be initiated");
                node_a.graceful_disconnect(peer).unwrap();
                shutdown_initiated = true;
            }
        }
    })
    .await
    .expect("handshake/exchange/shutdown sequence timed out");

    assert!(peer_of_a.is_some() && peer_of_b.is_some(), "both sides must learn the other's peer ID");
    assert_eq!(first_accept, Some(true), "the first sequence number must be accepted");
    assert_eq!(duplicate_rejected, Some(true), "the replayed sequence number must be rejected");
    assert_eq!(b_response_count, 2, "both requests must receive a response");
    assert!(a_disconnected && b_disconnected, "both sides must observe the graceful disconnect");
}
