//! Snapshot methods (OFS-1300). Metadata only — the bytes themselves are
//! served by `openfiat_snapshot::serve`'s `GET /snapshot/{id}`, merged
//! into the same axum router as these methods (see `crate::server`).
//!
//! `import` deliberately has no method here. Importing replaces this
//! node's entire worldview, and a node decides for itself when to do that
//! — on startup with no checkpoint, from an announcement it verified
//! itself (`actor::poll_snapshot_bootstrap`). Exposing it as an RPC would
//! hand any caller that decision; every `getX` below is a read of what
//! this node already believes, and `sendSnapshotAnnounce` only relays a
//! payload the caller's own wallet signed.

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
        "getCheckpointSlot",
        method_fn(
            |state: &NodeState<S>, _params: serde_json::Value| -> Result<Option<u64>, RpcError> {
                Ok(state.snapshots.checkpoint_slot())
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
