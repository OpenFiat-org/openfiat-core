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

    /// This node's identity in libp2p's own form, whose `Display` is the
    /// base58 `12D3Koo…` string that appears in a multiaddr's `/p2p/`
    /// segment. [`Node::local_peer_id`]'s protocol-level `PeerId` is the
    /// same identity as raw bytes, which is the wrong shape for anything
    /// an operator has to read or type.
    pub fn libp2p_peer_id(&self) -> Libp2pPeerId {
        *self.swarm.local_peer_id()
    }

    /// A handle for opening and accepting raw streams — how this node
    /// speaks bitswap without a second process holding a second identity.
    ///
    /// Cloneable and independent of `&mut self`, so the code that serves
    /// content does not have to reach through the swarm. `openfiat_content`
    /// names the protocol and decides what travels over it; the transport
    /// layer's only job is to carry it.
    pub fn content_control(&self) -> libp2p_stream::Control {
        self.swarm.behaviour().content.new_control()
    }

    /// Joins the public IPFS DHT, so this node's provider records reach
    /// peers other than the ones it already knows.
    ///
    /// Returns how many bootstrap addresses were added. Zero means every
    /// bootstrapper failed to resolve, which is a node that will publish
    /// nothing and should say so rather than look healthy.
    ///
    /// Separate from [`Node::new`] on purpose. Constructing the behaviour
    /// commits a node to nothing — publishing announces its peer id and
    /// addresses to the whole IPFS network, which is the point and also a
    /// disclosure an operator is entitled to decline. The node calls this
    /// only when they have not.
    /// Seed the DHT routing table with this node's own entrypoints.
    ///
    /// The public bootstrapper list is empty now that the DHT is private,
    /// so the peers an OpenFiat node already dials are the only ones there
    /// are to seed from — which is correct rather than a compromise: they
    /// are exactly the peers that speak this protocol.
    pub fn seed_content_routing(&mut self, peers: &[(Libp2pPeerId, libp2p::Multiaddr)]) -> usize {
        for (peer, address) in peers {
            self.swarm
                .behaviour_mut()
                .content_routing
                .add_address(peer, address.clone());
        }
        if !peers.is_empty() {
            let _ = self.swarm.behaviour_mut().content_routing.bootstrap();
        }
        peers.len()
    }

    pub fn join_content_routing(&mut self) -> usize {
        let mut added = 0;
        for (peer, address) in crate::content_routing::resolved_bootstrappers() {
            self.swarm
                .behaviour_mut()
                .content_routing
                .add_address(&peer, address);
            added += 1;
        }
        if added > 0 {
            // Fails only with an empty routing table, which `added > 0`
            // has just ruled out.
            let _ = self.swarm.behaviour_mut().content_routing.bootstrap();
        }
        added
    }

    /// Announces `address` as somewhere this node can be reached.
    ///
    /// Provider records carry the addresses the swarm has confirmed, so a
    /// node that publishes without one is telling the network it has
    /// content and giving nobody a way to ask for it. The caller decides
    /// what counts as confirmed — an operator's declaration, or an
    /// address enough independent peers have reported — because this
    /// layer cannot tell a genuine observation from a peer's suggestion.
    pub fn announce_address(&mut self, address: Multiaddr) {
        self.swarm.add_external_address(address);
    }

    /// Publishes that this node provides `key`, an IPFS multihash.
    ///
    /// A multihash rather than a CID: see `openfiat_crypto::Cid::multihash`
    /// for why the DHT keys content that way and what announcing the full
    /// CID would cost. The query runs in the background and republishes
    /// itself until [`Node::stop_providing`]; `false` means the local
    /// provider store is full, which is a node holding more than it can
    /// announce rather than a failure to reach anyone.
    pub fn start_providing(&mut self, key: &[u8]) -> bool {
        self.swarm
            .behaviour_mut()
            .content_routing
            .start_providing(libp2p::kad::RecordKey::new(&key))
            .is_ok()
    }

    /// Withdraws this node's claim to provide `key`.
    ///
    /// Local only, and that is not a gap: a provider record expires from
    /// the network on its own, and there is no message in the protocol
    /// for taking one back. What this stops is the republishing that
    /// would otherwise keep renewing a claim about content this node has
    /// evicted.
    pub fn stop_providing(&mut self, key: &[u8]) {
        self.swarm
            .behaviour_mut()
            .content_routing
            .stop_providing(&libp2p::kad::RecordKey::new(&key));
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
