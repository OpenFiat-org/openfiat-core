//! `openfiat-gossip` — Epidemic message propagation across the peer-to-peer network.
//!
//! Implements OFS-1200 (OGP) on top of `openfiat_network`: deterministic
//! event IDs (§5), origination authorization (§7), the validate→store→
//! broadcast lifecycle (§8-9), a RocksDB-backed dedup store (§10-11),
//! TTL-bounded forwarding (§12-13), selective channel subscription
//! (§18-19), and recovery on reconnect (§17, §22). Every provider crate
//! above this one (advertisements, registry, notifications, oracles,
//! risk) originates and consumes events through this crate rather than
//! talking to `openfiat_network` directly.

pub mod authorization;
pub mod channel;
pub mod error;
pub mod event_id;
pub mod protocol;
pub mod service;
pub mod store;

pub use channel::{Channel, Subscription};
pub use error::GossipError;
pub use service::{GossipService, ReceiveOutcome};
pub use store::EventStore;

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
