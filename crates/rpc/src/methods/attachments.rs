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
//! never substitute it. That is the path evidence in a dispute travels,
//! and it stays the only one that is checkable end to end.
//!
//! # `getContentFile`, and why an unverifiable read exists beside it
//!
//! `getContentFile` walks the DAG on this node and returns the assembled
//! file: one call instead of forty, and the caller cannot check the
//! result, because a dag-pb root's digest covers the root node and not
//! the bytes. Everything the paragraph above says against that is still
//! true. It exists because of who asks.
//!
//! A browser rendering `<img src="…">` performs no verification at any
//! point, through any gateway, ever. It was previously pointed at a
//! *public* IPFS gateway — a stranger, assembling the same DAG, with the
//! same inability to be checked, plus the disclosure of which OpenFiat
//! attachment is being looked at and by whom. Since this node's content
//! is no longer published to the public DHT, that gateway cannot resolve
//! it at all. So the choice is not between a verified read and an
//! unverified one; it is between an unverified read from the node the
//! client already selected and trusts for every other answer, and an
//! unverified read from somebody else. [`crate::gateway`] is the HTTP
//! shape of this method, and the reason it is worth having.

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

/// A caller handing this node bytes to hold, with the CID they believe
/// those bytes have.
#[derive(serde::Deserialize)]
pub struct PutContentParams {
    pub cid: String,
    /// Base64, one block. A file larger than a block is uploaded as its
    /// blocks and its dag-pb root, in separate calls — the same shape
    /// `getHeldContent` serves them back in, and for the same reason:
    /// every unit that moves is one the caller can check by itself.
    pub content: String,
}

#[derive(serde::Serialize)]
pub struct PutContentResponse {
    /// The CID this node stored the bytes under — always the one it was
    /// given, since bytes that do not hash to it are refused rather than
    /// stored somewhere else.
    pub cid: String,
    /// False when this node was already holding it. Not an error: an
    /// interface retrying an upload should get the same answer as the
    /// first time.
    pub stored: bool,
}

#[derive(serde::Serialize)]
pub struct HeldContentResponse {
    /// Base64. Absent when this node does not hold the content, which is
    /// an ordinary answer — most nodes hold nothing — and not an error.
    pub content: Option<String>,
}

#[derive(serde::Serialize)]
pub struct ContentFileResponse {
    /// Base64 of the whole file, chunks concatenated in DAG order.
    /// Absent when any block of it is missing — half a file is not a
    /// smaller file.
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
        "getContentFile",
        method_fn(
            |state: &NodeState<S>, params: CidParams| -> Result<ContentFileResponse, RpcError> {
                let cid = openfiat_crypto::Cid::parse(&params.cid)
                    .map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                match openfiat_content::dag::assemble(&cid, |block| state.held_content.get(block)) {
                    Ok(file) => Ok(ContentFileResponse {
                        content: Some(BASE64.encode(file)),
                    }),
                    // A block this node does not have. Ordinary — the same
                    // answer `getHeldContent` gives, for the same reason.
                    Err(openfiat_content::PinError::Unavailable(_)) => {
                        Ok(ContentFileResponse { content: None })
                    }
                    // Not ordinary: the CID names something this node
                    // holds and cannot honestly serve as a file. Saying
                    // "not held" would blame the network for a fault in
                    // the content.
                    Err(e) => Err(RpcError::InvalidParams(e.to_string())),
                }
            },
        ),
    );
    table.register(
        "sendContentPut",
        method_fn(
            |state: &NodeState<S>,
             params: PutContentParams|
             -> Result<PutContentResponse, RpcError> {
                let cid = openfiat_crypto::Cid::parse(&params.cid)
                    .map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                let content = BASE64
                    .decode(params.content.as_bytes())
                    .map_err(|e| RpcError::InvalidParams(e.to_string()))?;

                let already = state.held_content.holds(&cid);
                // `keep` recomputes the digest and refuses bytes that are
                // not what the CID names. That check is what makes an
                // open ingress safe to expose: a caller chooses what to
                // store but never chooses *where*, so nobody can overwrite
                // an existing CID with different bytes, and the worst an
                // abusive caller achieves is spending their own quota of
                // this node's retention window.
                //
                // Run unconditionally, including when the CID is already
                // held. Skipping it as an optimisation meant a mismatched
                // upload against known content returned success — the
                // caller was told their bytes were stored under a CID that
                // named somebody else's, which is the exact confusion this
                // check exists to prevent. `keep` is idempotent, so there
                // is nothing to save by skipping it.
                if !state.held_content.keep(&cid, &content) {
                    return Err(RpcError::InvalidParams(
                        "the content does not hash to the CID given, or exceeds the \
                         maximum block size"
                            .to_string(),
                    ));
                }

                // The retention sweep keeps what the records reference,
                // and an interface uploads before it publishes the record
                // that references. Without this note of when the bytes
                // arrived, a sweep landing in between throws them away —
                // see `NodeState::content_ingress`.
                state
                    .content_ingress
                    .borrow_mut()
                    .insert(cid.as_str().to_string(), openfiat_types::Timestamp::now());

                Ok(PutContentResponse {
                    cid: cid.as_str().to_string(),
                    stored: !already,
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
