//! Trade attachment methods.
//!
//! Two methods, and the asymmetry between them is the point.
//! `sendAttachmentPublish` takes a signed record and gossips it, like
//! every other `sendX`. `getSettlementAttachments` looks the settlement up
//! first, so it can pass the real buyer and seller to the registry — the
//! authorization check `openfiat_content::store` deliberately located on
//! the read path. A caller cannot ask for "all attachments" and filter
//! client-side, because that method does not exist.

use crate::dispatch::{IdParams, MethodTable, SendEventParams, decode_bytes, method_fn};
use crate::error::RpcError;
use crate::state::NodeState;
use openfiat_content::{Attachment, SignedAttachmentPublish, protocol};
use openfiat_serialization::{json, wire};
use openfiat_settlement::SettlementId;
use openfiat_storage::KvStore;
use openfiat_types::Priority;

pub fn register<S: KvStore + 'static>(table: &mut MethodTable<S>) {
    table.register(
        "getSettlementAttachments",
        method_fn(
            |state: &NodeState<S>, params: IdParams| -> Result<Vec<Attachment>, RpcError> {
                let id = SettlementId::new(params.id);
                // An unknown settlement returns an empty list rather than
                // an error. This node may simply not have replicated it
                // yet, and "no attachments" is the truthful answer to
                // what was asked; reporting "not found" would invite a
                // client to treat a replication lag as a permanent state.
                let Some(settlement) = state.settlements.get(&id) else {
                    return Ok(Vec::new());
                };
                Ok(state
                    .attachments
                    .find_by_settlement(&id, &[settlement.buyer, settlement.seller]))
            },
        ),
    );
    table.register(
        "sendAttachmentPublish",
        method_fn(
            |state: &NodeState<S>, params: SendEventParams| -> Result<String, RpcError> {
                let bytes = decode_bytes(&params.data)?;
                let signed: SignedAttachmentPublish =
                    json::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                let gossip_bytes =
                    wire::to_bytes(&signed).expect("SignedAttachmentPublish always serializes");
                let id = state
                    .attachments
                    .apply_publish(signed)
                    .map_err(|e| RpcError::Application(e.code()))?;
                crate::dispatch::originate(
                    state,
                    protocol::EVENT_PUBLISHED,
                    protocol::OFS_SPEC,
                    Priority::Reputation,
                    gossip_bytes,
                );
                Ok(id.as_str().to_string())
            },
        ),
    );
}
