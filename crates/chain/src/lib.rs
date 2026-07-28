//! `openfiat-chain` — the bridge to the Solana execution layer OFS-4200's
//! on-chain programs run on (OFS-4300).
//!
//! An RPC-connected node ([`RpcChainClient`]) supplies a blockhash and
//! submits transactions directly. A gossip-only node has neither — it
//! gets both from RPC-connected peers over gossip instead, using
//! [`BlockhashCache`] for the blockhash side (the gossip wiring itself,
//! and the transaction-relay request/response path, land in the next
//! phase alongside `openfiat-gossip`'s new Chain channel).
//!
//! Neither path ever holds or signs with a user's Solana key — every
//! transaction arrives here already signed; this crate only supplies
//! blockhashes and forwards signed bytes (OFS-4300 §5).

mod blockhash;
mod client;
mod error;
mod mode;

pub use blockhash::{BLOCKHASH_VALIDITY, BlockhashCache};
pub use client::{ChainClient, RpcChainClient, SignatureStatus};
pub use error::ChainError;
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
