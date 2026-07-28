//! Node-level methods — Solana's `getVersion`/`getHealth` equivalents.

use crate::dispatch::{MethodTable, method_fn};
use crate::error::RpcError;
use crate::state::NodeState;
use openfiat_storage::KvStore;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct VersionResult {
    pub version: &'static str,
}

pub fn register<S: KvStore + 'static>(table: &mut MethodTable<S>) {
    table.register(
        "getVersion",
        method_fn(|_state: &NodeState<S>, _params: serde_json::Value| -> Result<VersionResult, RpcError> { Ok(VersionResult { version: crate::version() }) }),
    );
    table.register("getHealth", method_fn(|_state: &NodeState<S>, _params: serde_json::Value| -> Result<&'static str, RpcError> { Ok("ok") }));
}
