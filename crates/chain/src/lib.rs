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
pub mod mints;
mod mode;
pub mod programs;
pub mod protocol;
mod state;
mod validate;

pub use blockhash::{BLOCKHASH_VALIDITY, BlockhashCache};
pub use client::{ChainClient, RpcChainClient, SignatureStatus, validate_rpc_endpoint};
pub use error::ChainError;
pub use gossip_service::{ChainBridge, ChainGossipService};
pub use mints::{KNOWN_MINTS, KnownMint, symbol_for_mint};
pub use mode::NodeChainMode;
// The deployed programs this build is pinned to — protocol identity, fixed
// at compile time and deliberately not configurable; see `programs`.
pub use programs::{IDS as PROGRAM_IDS, ProgramIds};
// `PendingRelay`/`AwaitingConfirmation` are returned by `ChainState`'s own
// public methods, so they belong in the public API too — without this a
// caller can consume them by inference but cannot name them in a signature.
pub use state::{AwaitingConfirmation, ChainState, PendingRelay};
pub use validate::validate_transaction_bytes;

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
