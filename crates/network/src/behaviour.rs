//! The combined libp2p behaviour a node runs.
//!
//! Three protocols share the one multiplexed connection (OFNP §20):
//! `identify` (peer capability/version advertisement, §11), `ping`
//! (session liveness, §18 — see [`crate::heartbeat`]), and our own
//! envelope request-response protocol (§13-16).

use crate::envelope::{EnvelopeCodec, PROTOCOL};
use crate::heartbeat;
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
}

impl OpenFiatBehaviour {
    pub fn new(local_public_key: libp2p::identity::PublicKey) -> Self {
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
            identify,
            ping,
            envelope,
        }
    }
}
