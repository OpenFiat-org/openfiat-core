//! Snapshot methods (OFS-1300). `import` isn't exposed here — actual
//! snapshot bytes travel over whatever transport a client chooses (§14),
//! not this JSON-RPC surface; a client downloads out of band and calls
//! `openfiat_snapshot::SnapshotIndex::import` directly.

use crate::dispatch::{IdParams, MethodTable, SendEventParams, decode_bytes, method_fn};
use crate::error::RpcError;
use crate::state::NodeState;
use openfiat_serialization::{json, wire};
use openfiat_snapshot::events::SignedSnapshotAnnounce;
use openfiat_snapshot::{SnapshotId, SnapshotMetadata, protocol};
use openfiat_storage::KvStore;
use openfiat_types::Priority;

pub fn register<S: KvStore + 'static>(table: &mut MethodTable<S>) {
    table.register(
        "getSnapshot",
        method_fn(
            |state: &NodeState<S>,
             params: IdParams|
             -> Result<Option<SnapshotMetadata>, RpcError> {
                Ok(state.snapshots.get(&SnapshotId::new(params.id)))
            },
        ),
    );
    table.register(
        "getSnapshots",
        method_fn(
            |state: &NodeState<S>,
             _params: serde_json::Value|
             -> Result<Vec<SnapshotMetadata>, RpcError> { Ok(state.snapshots.all()) },
        ),
    );
    table.register(
        "getLatestSnapshot",
        method_fn(
            |state: &NodeState<S>,
             _params: serde_json::Value|
             -> Result<Option<SnapshotMetadata>, RpcError> {
                Ok(state.snapshots.latest())
            },
        ),
    );
    table.register(
        "getCheckpointHeight",
        method_fn(
            |state: &NodeState<S>, _params: serde_json::Value| -> Result<Option<u64>, RpcError> {
                Ok(state.snapshots.checkpoint_height())
            },
        ),
    );
    table.register(
        "sendSnapshotAnnounce",
        method_fn(
            |state: &NodeState<S>, params: SendEventParams| -> Result<String, RpcError> {
                let bytes = decode_bytes(&params.data)?;
                let signed: SignedSnapshotAnnounce =
                    json::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                let gossip_bytes =
                    wire::to_bytes(&signed).expect("SignedSnapshotAnnounce always serializes");
                let id = state
                    .snapshots
                    .apply_announce(signed)
                    .map_err(|e| RpcError::Application(e.code()))?;
                crate::dispatch::originate(
                    state,
                    protocol::EVENT_ANNOUNCED,
                    protocol::OFS_SPEC,
                    Priority::Snapshot,
                    gossip_bytes,
                );
                Ok(id.as_str().to_string())
            },
        ),
    );
}
