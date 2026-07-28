//! `openfiat-discovery` — Peer discovery: bootstrap, routing table, peer exchange.
//!
//! Implements OFS-1100 (PDP) on top of `openfiat_network`: a persistent
//! peer cache (§7), signed advertisements (§8), peer exchange carried as
//! `openfiat_network::Envelope` payloads (§9), reconnection backoff (§15),
//! and bootstrap-independence policy (§6, §17). Gossip and everything
//! above it (OFS-1200 onward) connects to the peer set this crate builds.

pub mod advertisement;
pub mod backoff;
pub mod bootstrap;
pub mod cache;
pub mod error;
pub mod exchange;
pub mod record;
pub mod service;

pub use advertisement::{Advertisement, SignedAdvertisement};
pub use cache::PeerCache;
pub use error::DiscoveryError;
pub use record::PeerRecord;
pub use service::DiscoveryService;

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
