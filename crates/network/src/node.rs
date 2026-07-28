//! A running OpenFiat node's libp2p transport (OFNP §8-9, §23).
//!
//! Deliberately thin: `Node` owns the `Swarm` and exposes the handful of
//! operations Phase 2 and its callers need (listen, dial, send an envelope,
//! read the next event, disconnect gracefully). Higher-level session
//! bookkeeping — sequence tracking per peer, connection-lifecycle state,
//! heartbeat-driven eviction — belongs to whichever crate consumes these
//! events, not to the transport wrapper itself.

use crate::behaviour::{OpenFiatBehaviour, OpenFiatBehaviourEvent};
use crate::envelope::Envelope;
use crate::error::NetworkError;
use crate::heartbeat;
use crate::identity::{from_libp2p_peer_id, to_libp2p_keypair};
use futures::StreamExt;
use libp2p::request_response::OutboundRequestId;
use libp2p::swarm::SwarmEvent;
use libp2p::{Multiaddr, PeerId as Libp2pPeerId, Swarm, noise, tcp, yamux};
use openfiat_crypto::Keypair;
use openfiat_types::PeerId;

/// A node's libp2p transport: TCP+Noise+Yamux as a fallback alongside QUIC
/// (which brings its own TLS-based security — OFNP §5's "Security (Noise)"
/// applies to the TCP path; QUIC's security is inherent to the transport).
pub struct Node {
    pub swarm: Swarm<OpenFiatBehaviour>,
}

impl Node {
    /// Build a node's transport from its Ed25519 keypair (also its OFNP
    /// §6 signing identity — see [`crate::identity`]).
    pub fn new(keypair: &Keypair) -> Result<Self, NetworkError> {
        let swarm = libp2p::SwarmBuilder::with_existing_identity(to_libp2p_keypair(keypair))
            .with_tokio()
            .with_tcp(
                tcp::Config::default(),
                noise::Config::new,
                yamux::Config::default,
            )
            .map_err(|_| NetworkError::Internal)?
            .with_quic()
            .with_behaviour(|key| OpenFiatBehaviour::new(key.public()))
            .map_err(|_| NetworkError::Internal)?
            // libp2p-swarm's own default (10s) is shorter than our ping
            // interval (`heartbeat::INTERVAL`, 15s): a connection with no
            // application traffic between heartbeats would idle-time-out
            // before the first ping ever had a chance to keep it alive.
            .with_swarm_config(|config| config.with_idle_connection_timeout(heartbeat::TIMEOUT))
            .build();
        Ok(Self { swarm })
    }

    pub fn local_peer_id(&self) -> PeerId {
        from_libp2p_peer_id(*self.swarm.local_peer_id())
    }

    pub fn listen_on(&mut self, addr: Multiaddr) -> Result<(), NetworkError> {
        self.swarm
            .listen_on(addr)
            .map(|_| ())
            .map_err(|_| NetworkError::Internal)
    }

    pub fn dial(&mut self, addr: Multiaddr) -> Result<(), NetworkError> {
        self.swarm.dial(addr).map_err(|_| NetworkError::Internal)
    }

    /// Send an envelope to `peer` over the negotiated envelope protocol,
    /// returning an ID the eventual `Event::Message` response correlates to.
    pub fn send_envelope(&mut self, peer: Libp2pPeerId, envelope: Envelope) -> OutboundRequestId {
        self.swarm
            .behaviour_mut()
            .envelope
            .send_request(&peer, envelope)
    }

    /// Wait for the next swarm event. Exposed as libp2p's own event type
    /// rather than a bespoke wrapper — see the module doc for why.
    pub async fn next_event(&mut self) -> SwarmEvent<OpenFiatBehaviourEvent> {
        self.swarm.select_next_some().await
    }

    /// OFNP §23 graceful shutdown, as far as the transport layer is
    /// concerned: notify the peer and close the session. ("Stop accepting
    /// requests" / "complete outstanding responses" are the caller's
    /// responsibility before it calls this; "close transport" happens when
    /// the `Node` is dropped.)
    pub fn graceful_disconnect(&mut self, peer: Libp2pPeerId) -> Result<(), NetworkError> {
        self.swarm
            .disconnect_peer_id(peer)
            .map_err(|_| NetworkError::Internal)
    }
}
