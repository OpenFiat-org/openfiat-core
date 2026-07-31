//! Trade attachment methods.
//!
//! Two methods, and the asymmetry between them is the point.
//! `sendAttachmentPublish` takes a signed record and gossips it, like
//! every other `sendX`. `getSettlementAttachments` looks the settlement up
//! first, so it can pass the real buyer and seller to the registry — the
//! authorization check `openfiat_content::store` deliberately located on
//! the read path. A caller cannot ask for "all attachments" and filter
//! client-side, because that method does not exist.
//!
//! # `getHeldContent` is also the interface's fallback, not only a
//! challenger's question
//!
//! An OpenFiat client reads attachments through a public IPFS gateway.
//! When that gateway does not have one — which is precisely the case the
//! durability premium is paid to survive — this is where the bytes come
//! from instead: the access node the client already selected, over the
//! JSON-RPC it is already speaking, with the answer hashed against the
//! CID it asked for. Nothing about the node has to be trusted for that to
//! be safe, which is the whole property of a content address.
//!
//! Blocks rather than files, deliberately. Under 256 KiB the two are the
//! same thing. Above it a CID names a dag-pb root, and a node that
//! assembled the file and returned *that* would be handing back bytes the
//! caller cannot check, since the root's digest covers the DAG node and
//! not the file. So a client walks: fetch the root, hash it, read its
//! links, fetch each linked block, hash each one. Every step is checked
//! against a CID the client either brought with it or read out of a block
//! it had already verified, so a dishonest node can withhold content but
//! never substitute it. A `getAttachmentFile` returning assembled bytes
//! would be one call instead of forty and unverifiable, which for
//! evidence in a dispute is the wrong trade.

use crate::dispatch::{IdParams, MethodTable, SendEventParams, decode_bytes, method_fn};
use crate::error::RpcError;
use crate::state::NodeState;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use openfiat_content::{Attachment, SignedAttachmentPublish, protocol};
use openfiat_serialization::{json, wire};
use openfiat_settlement::SettlementId;
use openfiat_storage::KvStore;
use openfiat_types::Priority;

/// A caller naming the content it wants. Not an attachment id: a
/// challenger picks a CID from records it already holds and asks whether
/// this node can serve *that content*, which is the question the reward
/// premium turns on.
#[derive(serde::Deserialize)]
pub struct CidParams {
    pub cid: String,
}

#[derive(serde::Serialize)]
pub struct HeldContentResponse {
    /// Base64. Absent when this node does not hold the content, which is
    /// an ordinary answer — most nodes hold nothing — and not an error.
    pub content: Option<String>,
}

pub fn register<S: KvStore + 'static>(table: &mut MethodTable<S>) {
    table.register(
        "getHeldContent",
        method_fn(
            |state: &NodeState<S>, params: CidParams| -> Result<HeldContentResponse, RpcError> {
                // Parsed, not trusted. The string arrives from anyone, and
                // it is about to become a store key; `Cid::parse` is what
                // stops a caller probing this node's storage with a
                // crafted key instead of a content address.
                let cid = openfiat_crypto::Cid::parse(&params.cid)
                    .map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                Ok(HeldContentResponse {
                    content: state
                        .held_content
                        .get(&cid)
                        .map(|bytes| BASE64.encode(bytes)),
                })
            },
        ),
    );
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
