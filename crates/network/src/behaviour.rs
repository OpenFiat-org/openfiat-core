//! The combined libp2p behaviour a node runs.
//!
//! Five protocols share the one multiplexed connection (OFNP §20):
//! `identify` (peer capability/version advertisement, §11), `ping`
//! (session liveness, §18 — see [`crate::heartbeat`]), our own envelope
//! request-response protocol (§13-16), `content` — raw streams, used to
//! speak bitswap so this node serves IPFS content itself rather than
//! through a separate daemon with a separate identity — and
//! `content_routing`, the public IPFS DHT, which is how anyone finds out
//! this node has the content in the first place.
//!
//! `content` is a stream behaviour rather than another request-response
//! one because bitswap is not request/response: a peer writes a message
//! and closes, and the answer comes back over a fresh stream in the other
//! direction. Nothing here knows what bitswap is — the behaviour opens
//! and accepts streams on a named protocol, and `openfiat_content` says
//! what travels over them.

use crate::envelope::{EnvelopeCodec, PROTOCOL};
use crate::heartbeat;
use libp2p::kad::{self, store::MemoryStore};
use libp2p::request_response::ProtocolSupport;
use libp2p::swarm::NetworkBehaviour;
use libp2p::{identify, ping, request_response};
use std::iter;

/// The user-agent string OpenFiat nodes identify themselves with.
pub const AGENT_VERSION: &str = concat!("openfiat/", env!("CARGO_PKG_VERSION"));

#[derive(NetworkBehaviour)]
pub struct OpenFiatBehaviour {
    pub identify: identify::Behaviour,
    pub ping: ping::Behaviour,
    pub envelope: request_response::Behaviour<EnvelopeCodec>,
    pub content: libp2p_stream::Behaviour,
    /// The public IPFS DHT, as a client — how the content this node
    /// serves becomes findable by anyone who never heard of OpenFiat.
    /// See [`crate::content_routing`].
    ///
    /// Always constructed, never automatically active: in client mode it
    /// answers no queries, and with an empty routing table and nothing
    /// declared as provided it publishes nothing either. Joining the DHT
    /// is [`crate::node::Node::join_content_routing`], which the node
    /// calls only when its operator has not declined — a behaviour that
    /// exists is not a node that has announced itself.
    pub content_routing: kad::Behaviour<MemoryStore>,
}

impl OpenFiatBehaviour {
    pub fn new(local_public_key: libp2p::identity::PublicKey) -> Self {
        let local_peer_id = local_public_key.to_peer_id();
        let identify = identify::Behaviour::new(
            identify::Config::new("/openfiat/id/1.0.0".to_string(), local_public_key)
                .with_agent_version(AGENT_VERSION.to_string()),
        );

        let ping = ping::Behaviour::new(
            ping::Config::new()
                .with_interval(heartbeat::INTERVAL)
                .with_timeout(heartbeat::TIMEOUT),
        );

        let envelope = request_response::Behaviour::new(
            iter::once((PROTOCOL, ProtocolSupport::Full)),
            request_response::Config::default(),
        );

        Self {
            content_routing: crate::content_routing::behaviour(local_peer_id),
            identify,
            ping,
            envelope,
            content: libp2p_stream::Behaviour::new(),
        }
    }
}
