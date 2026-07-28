//! The three OFS-4300 gossip events (registered in OFS-8100 §18).
//!
//! Unlike most domain events on this network, none of these carry their
//! own `Signed*` wrapper on top of gossip's generic envelope signature.
//! That pattern exists elsewhere to let a payload be verified in
//! isolation once extracted from its envelope and persisted (an
//! `OracleRecord` needs to keep proving its provenance long after the
//! transport envelope that carried it is gone). Nothing here is durable
//! state in that sense:
//!
//! - `BlockhashAnnounced` is a claim any RPC-connected peer can trivially
//!   falsify-check by trying to use the blockhash — a false claim just
//!   fails on Solana's own terms, harmlessly.
//! - `TransactionRelayRequested` carries bytes that are *already* signed,
//!   by the Solana cluster's own signing scheme (the transaction's
//!   sender's keypair) — that signature, not an OpenFiat one, is what
//!   actually matters, and it's verified by the cluster, not by us.
//! - `TransactionRelayed` is an explicitly best-effort confirmation echo
//!   (OFS-4300 §7) that nothing's correctness depends on.
//!
//! Gossip's own envelope signature (tying each event to the `PeerId`
//! that originated it) is authentication enough for all three.

use openfiat_types::Timestamp;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BlockhashAnnounced {
    pub blockhash: String,
    pub slot: u64,
    pub observed_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TransactionRelayRequested {
    /// The transaction's own signed wire bytes (bincode, matching
    /// whatever `solana_transaction::versioned::VersionedTransaction`
    /// serializes to) — already signed by its sender, not by this node.
    pub tx_bytes: Vec<u8>,
    pub requested_at: Timestamp,
    /// Opaque caller-supplied correlation tag (e.g. a settlement ID) —
    /// this crate never interprets it, only carries it from the
    /// originating node's `sendTransaction` call through to whichever
    /// peer ends up actually submitting and confirming it, so that
    /// node's own local domain registries (already converged via gossip)
    /// can react once real on-chain confirmation is observed.
    pub correlation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TransactionRelayed {
    pub signature: String,
    pub slot_submitted: u64,
}
