//! Shared plumbing every `methods::*` module builds on: the method
//! table itself, a `typed` helper that turns a `fn(P) -> Result<R,
//! RpcError>` into the `Fn(&NodeState<S>, Value) -> Result<Value,
//! RpcError>` shape the table stores, and the handful of parameter
//! shapes reused across nearly every domain (an opaque ID, a wallet, a
//! pre-signed event's wire bytes).

use crate::error::RpcError;
use crate::state::NodeState;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use openfiat_storage::KvStore;
use openfiat_types::{EventType, PeerId, Priority};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::HashMap;

pub type MethodFn<S> = Box<dyn Fn(&NodeState<S>, Value) -> Result<Value, RpcError>>;

#[derive(Default)]
pub struct MethodTable<S> {
    methods: HashMap<&'static str, MethodFn<S>>,
}

impl<S: KvStore + 'static> MethodTable<S> {
    pub fn new() -> Self {
        Self {
            methods: HashMap::new(),
        }
    }

    pub fn register(&mut self, name: &'static str, method: MethodFn<S>) {
        let previous = self.methods.insert(name, method);
        assert!(
            previous.is_none(),
            "duplicate RPC method registration: {name}"
        );
    }

    pub fn dispatch(
        &self,
        state: &NodeState<S>,
        method: &str,
        params: Value,
    ) -> Result<Value, RpcError> {
        match self.methods.get(method) {
            Some(handler) => handler(state, params),
            None => Err(RpcError::MethodNotFound(method.to_string())),
        }
    }

    pub fn method_names(&self) -> Vec<&'static str> {
        let mut names: Vec<&'static str> = self.methods.keys().copied().collect();
        names.sort_unstable();
        names
    }
}

/// The bridge used by every `methods::*` function: builds a
/// [`MethodFn`] that deserializes its params as `P` and serializes `f`'s
/// result back to JSON, mapping deserialization failure to
/// [`RpcError::InvalidParams`].
pub fn method_fn<S, P, R>(
    f: impl Fn(&NodeState<S>, P) -> Result<R, RpcError> + 'static,
) -> MethodFn<S>
where
    S: 'static,
    P: DeserializeOwned + 'static,
    R: Serialize + 'static,
{
    Box::new(move |state, params| {
        let params: P =
            serde_json::from_value(params).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
        let result = f(state, params)?;
        serde_json::to_value(result).map_err(|e| RpcError::Internal(e.to_string()))
    })
}

pub fn encode_bytes(bytes: &[u8]) -> String {
    BASE64.encode(bytes)
}

pub fn decode_bytes(encoded: &str) -> Result<Vec<u8>, RpcError> {
    BASE64
        .decode(encoded)
        .map_err(|e| RpcError::InvalidParams(format!("invalid base64: {e}")))
}

pub fn decode_peer_id(encoded: &str) -> Result<PeerId, RpcError> {
    Ok(PeerId::from_bytes(decode_bytes(encoded)?))
}

pub fn encode_peer_id(peer_id: &PeerId) -> String {
    encode_bytes(peer_id.as_bytes())
}

/// Re-broadcasts an already-applied signed payload through this node's
/// gossip — every `sendX` handler calls this *after* applying the
/// payload to its own registry directly (so a rejected submission still
/// gets a real `RpcError` back to the caller, which a silently-discarded
/// gossip-handler failure never would). `event_type`/`ofs_spec`/
/// `priority` mirror the exact values each domain crate's own
/// `*Service::originate` already uses; `bytes` is the domain's wire
/// encoding of the same payload just applied, computed by the caller
/// before that payload was moved into its own `apply_*` call.
///
/// Fire-and-forget: a node missing the role a handful of event types
/// require to originate (`openfiat_gossip::authorization`) still keeps
/// the write it just applied locally, it just doesn't propagate — a
/// degraded outcome, not a fatal one for the caller.
pub fn originate<S: KvStore + 'static>(
    state: &NodeState<S>,
    event_type: &str,
    ofs_spec: u16,
    priority: Priority,
    bytes: Vec<u8>,
) {
    if let Ok(event_type) = EventType::new(event_type) {
        let _ = state
            .gossip
            .borrow_mut()
            .originate(event_type, ofs_spec, priority, 8, bytes);
    }
}

/// Params shared by nearly every `getX(id)` method.
#[derive(Debug, serde::Deserialize)]
pub struct IdParams {
    pub id: String,
}

/// Params shared by every `getXByWallet`/`getReputation`-style method —
/// `wallet` is a base64-encoded `PeerId`.
#[derive(Debug, serde::Deserialize)]
pub struct WalletParams {
    pub wallet: String,
}

/// Params for every `sendX` mutation: `data` is the base64-encoded,
/// already-signed wire payload the caller's own wallet produced —
/// mirroring Solana's `sendTransaction`, this crate never constructs or
/// signs anything on the caller's behalf.
#[derive(Debug, serde::Deserialize)]
pub struct SendEventParams {
    pub data: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfiat_storage::mem::MemoryStore;

    #[test]
    fn dispatch_reports_method_not_found() {
        let table: MethodTable<MemoryStore> = MethodTable::new();
        let state = NodeState::new_for_test(MemoryStore::new());
        let result = table.dispatch(&state, "doesNotExist", Value::Null);
        assert!(matches!(result, Err(RpcError::MethodNotFound(_))));
    }

    #[test]
    fn a_registered_method_dispatches_and_round_trips_json() {
        let mut table: MethodTable<MemoryStore> = MethodTable::new();
        table.register(
            "echo",
            method_fn(|_state: &NodeState<MemoryStore>, params: IdParams| Ok(params.id)),
        );
        let state = NodeState::new_for_test(MemoryStore::new());
        let result = table
            .dispatch(&state, "echo", serde_json::json!({ "id": "hello" }))
            .unwrap();
        assert_eq!(result, Value::from("hello"));
    }

    #[test]
    #[should_panic(expected = "duplicate RPC method registration")]
    fn registering_the_same_method_name_twice_panics() {
        let mut table: MethodTable<MemoryStore> = MethodTable::new();
        table.register(
            "dup",
            method_fn(|_state: &NodeState<MemoryStore>, _: IdParams| Ok(())),
        );
        table.register(
            "dup",
            method_fn(|_state: &NodeState<MemoryStore>, _: IdParams| Ok(())),
        );
    }

    #[test]
    fn peer_id_round_trips_through_base64() {
        let peer_id = PeerId::from_bytes(vec![1, 2, 3, 4]);
        assert_eq!(decode_peer_id(&encode_peer_id(&peer_id)).unwrap(), peer_id);
    }
}
