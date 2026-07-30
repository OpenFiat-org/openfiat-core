//! `openfiat-network` — Transport layer: TCP/QUIC, TLS abstraction, connection management.
//!
//! Implements OFS-1000 (OFNP): libp2p transport (Noise + QUIC + Yamux, per
//! `docs/architecture.md`), the connection lifecycle state machine (§8),
//! the message envelope (§13-16), per-session sequence tracking (§15), and
//! heartbeat-driven liveness (§18). Peer discovery, gossip, and everything
//! above them (OFS-1100 onward) build on this crate rather than reimplementing
//! any of it.

pub mod behaviour;
pub mod envelope;
pub mod error;
pub mod heartbeat;
pub mod identity;
pub mod lifecycle;
pub mod node;
pub mod sequence;

pub use envelope::{Envelope, EnvelopeCodec, Header};
pub use error::NetworkError;
pub use lifecycle::ConnectionState;
pub use node::Node;

/// The swarm event a caller routes on, and the request-response types an
/// envelope arrives in.
///
/// Re-exported for the same reason `Multiaddr` and `PeerId` are: a crate
/// that reads this node's network events should not have to take a direct
/// `libp2p` dependency, pin its own version, and risk holding a different
/// one from the swarm it is reading.
pub use libp2p::request_response;
pub use libp2p::swarm::SwarmEvent;
/// Re-exported so crates above this one don't need their own direct
/// `libp2p` dependency just to name an address or a peer.
pub use libp2p::{Multiaddr, PeerId};
pub use sequence::SequenceTracker;

/// Crate version, re-exported for diagnostics and `openfiat-node --version`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_a_version() {
        assert!(!version().is_empty());
    }
}
