//! `openfiat-disputes` — Dispute lifecycle and evidence handling.
//!
//! Implements OFS-2400 (ODP) / Ch.11's decentralized commit-reveal
//! arbitration on top of `openfiat_gossip` and `openfiat_settlement`:
//! dispute events (open/arbitrator-joined/vote-committed/vote-revealed/
//! mutual-settlement) travel as gossip events, and every node derives its
//! local dispute state — including consensus — purely by consuming them,
//! the same replication pattern used throughout this workspace. Actual
//! OPEN staking and slashing are Solana program operations this P2P layer
//! doesn't invoke yet (see the `record` module doc).

pub mod commitment;
pub mod error;
pub mod events;
pub mod protocol;
pub mod record;
pub mod service;
pub mod store;

pub use error::DisputeError;
pub use record::{Dispute, DisputeId, DisputeStatus, Resolution, Vote};
pub use service::DisputeService;
pub use store::DisputeRegistry;

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
