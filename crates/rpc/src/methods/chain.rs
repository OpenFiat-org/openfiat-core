//! Chain-bridge methods (OFS-4300 §8): `getChainStatus`, `getLatestBlockhash`,
//! `sendTransaction`. Identical behavior regardless of this node's own
//! `NodeChainMode` — callers never need to know or care which one it is.
//!
//! `sendTransaction`'s `data` is a base64-encoded, already-signed Solana
//! transaction's own wire bytes — not an OpenFiat `Signed*` JSON envelope,
//! the one documented exception to OFS-8200 §5's usual `sendX` shape
//! (see that section's own note). This crate's dispatch is synchronous
//! end to end, so this handler queues the (already-validated) bytes
//! rather than submitting them inline — see `ChainState`'s own doc for
//! why, and what actually drains that queue.

use crate::dispatch::{MethodTable, decode_bytes, method_fn};
use crate::error::RpcError;
use crate::state::NodeState;
use openfiat_storage::KvStore;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
pub struct ChainStatusResult {
    pub mode: &'static str,
    pub blockhash: Option<String>,
    pub slot: Option<u64>,
    pub age_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct LatestBlockhashResult {
    pub blockhash: String,
    pub slot: u64,
}

#[derive(Debug, Serialize)]
pub struct SendTransactionResult {
    pub queued: bool,
}

/// `correlation` is optional and opaque to `ChainState`/this crate's own
/// dispatch — it's carried through the pending-relay/awaiting-
/// confirmation tracking so that once this specific transaction is
/// observed as genuinely confirmed (not merely accepted for submission),
/// `poll_chain` can route a follow-up call to the right domain registry.
/// Convention (interpreted only by `actor::poll_chain`, not by this
/// module): `"settlement:<id>"` -> `SettlementRegistry::
/// apply_escrow_released`, `"dispute:<id>"` -> `DisputeRegistry::
/// apply_onchain_execution`. Not an OFS-4300-defined field — this
/// workspace's own extension for correlating a generic chain-bridge
/// relay with the domain event that triggered it.
#[derive(Debug, Deserialize)]
pub struct SendTransactionParams {
    pub data: String,
    #[serde(default)]
    pub correlation: Option<String>,
}

pub fn register<S: KvStore + 'static>(table: &mut MethodTable<S>) {
    table.register(
        "getChainStatus",
        method_fn(
            |state: &NodeState<S>,
             _params: serde_json::Value|
             -> Result<ChainStatusResult, RpcError> {
                let mode = if state.chain.mode().is_rpc_connected() {
                    "RpcConnected"
                } else {
                    "GossipOnly"
                };
                let (blockhash, slot) = match state.chain.current_blockhash() {
                    Some((hash, slot)) => (Some(hash), Some(slot)),
                    None => (None, None),
                };
                let age_ms = state
                    .chain
                    .current_blockhash_age()
                    .map(|age| age.as_millis() as u64);
                Ok(ChainStatusResult {
                    mode,
                    blockhash,
                    slot,
                    age_ms,
                })
            },
        ),
    );

    table.register(
        "getLatestBlockhash",
        method_fn(
            |state: &NodeState<S>,
             _params: serde_json::Value|
             -> Result<LatestBlockhashResult, RpcError> {
                state
                    .chain
                    .current_blockhash()
                    .map(|(blockhash, slot)| LatestBlockhashResult { blockhash, slot })
                    .ok_or(RpcError::Application(
                        openfiat_types::ErrorCode::ChainUnavailable,
                    ))
            },
        ),
    );

    table.register(
        "sendTransaction",
        method_fn(
            |state: &NodeState<S>,
             params: SendTransactionParams|
             -> Result<SendTransactionResult, RpcError> {
                let tx_bytes = decode_bytes(&params.data)?;
                state
                    .chain
                    .enqueue_relay(tx_bytes, params.correlation)
                    .map_err(|err| RpcError::Application(err.code()))?;
                Ok(SendTransactionResult { queued: true })
            },
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::{MethodTable, encode_bytes};
    use openfiat_storage::mem::MemoryStore;

    fn table_and_state() -> (MethodTable<MemoryStore>, NodeState<MemoryStore>) {
        let mut table = MethodTable::new();
        register(&mut table);
        (table, NodeState::new_for_test(MemoryStore::new()))
    }

    #[test]
    fn chain_status_reports_gossip_only_with_no_blockhash_by_default() {
        let (table, state) = table_and_state();
        let result = table
            .dispatch(&state, "getChainStatus", serde_json::json!({}))
            .unwrap();
        assert_eq!(result["mode"], "GossipOnly");
        assert!(result["blockhash"].is_null());
        assert!(result["age_ms"].is_null());
    }

    #[test]
    fn get_latest_blockhash_fails_with_chain_unavailable_before_anything_is_recorded() {
        let (table, state) = table_and_state();
        let err = table
            .dispatch(&state, "getLatestBlockhash", serde_json::json!({}))
            .unwrap_err();
        assert!(matches!(
            err,
            RpcError::Application(openfiat_types::ErrorCode::ChainUnavailable)
        ));
    }

    #[test]
    fn get_latest_blockhash_returns_a_recorded_blockhash() {
        let (table, state) = table_and_state();
        state.chain.record_blockhash("hash-xyz", 42);
        let result = table
            .dispatch(&state, "getLatestBlockhash", serde_json::json!({}))
            .unwrap();
        assert_eq!(result["blockhash"], "hash-xyz");
        assert_eq!(result["slot"], 42);

        let status = table
            .dispatch(&state, "getChainStatus", serde_json::json!({}))
            .unwrap();
        assert_eq!(status["blockhash"], "hash-xyz");
    }

    #[test]
    fn send_transaction_rejects_a_malformed_payload() {
        let (table, state) = table_and_state();
        let params = serde_json::json!({ "data": encode_bytes(&[1, 2, 3]) });
        let err = table
            .dispatch(&state, "sendTransaction", params)
            .unwrap_err();
        assert!(matches!(
            err,
            RpcError::Application(openfiat_types::ErrorCode::MalformedTransaction)
        ));
    }
}
