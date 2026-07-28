//! `openfiat-chain` — the bridge to the Solana execution layer OFS-4200's
//! on-chain programs run on (OFS-4300).
//!
//! An RPC-connected node ([`RpcChainClient`]) supplies a blockhash and
//! submits transactions directly. A gossip-only node has neither — it
//! gets both from RPC-connected peers over gossip instead, via
//! [`ChainGossipService`], which wires [`events`]' three event types onto
//! a node's shared `GossipService` and drives [`BlockhashCache`].
//!
//! Neither path ever holds or signs with a user's Solana key — every
//! transaction arrives here already signed; this crate only supplies
//! blockhashes and forwards signed bytes (OFS-4300 §5).

mod blockhash;
mod client;
mod error;
pub mod events;
mod gossip_service;
mod mode;
pub mod protocol;

pub use blockhash::{BLOCKHASH_VALIDITY, BlockhashCache};
pub use client::{ChainClient, RpcChainClient, SignatureStatus};
pub use error::ChainError;
pub use gossip_service::ChainGossipService;
pub use mode::NodeChainMode;

/// Crate version, re-exported for diagnostics.
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
