//! Dispute methods (OFS-2400).

use crate::dispatch::{IdParams, MethodTable, SendEventParams, decode_bytes, method_fn};
use crate::error::RpcError;
use crate::state::NodeState;
use openfiat_disputes::events::{
    SignedArbitratorJoin, SignedDisputeOpen, SignedVoteCommit, SignedVoteReveal,
};
use openfiat_disputes::{Dispute, DisputeId, protocol};
use openfiat_serialization::{json, wire};
use openfiat_storage::KvStore;
use openfiat_types::Priority;

pub fn register<S: KvStore + 'static>(table: &mut MethodTable<S>) {
    table.register(
        "getDispute",
        method_fn(
            |state: &NodeState<S>, params: IdParams| -> Result<Option<Dispute>, RpcError> {
                Ok(state.disputes.get(&DisputeId::new(params.id)))
            },
        ),
    );
    table.register(
        "getDisputes",
        method_fn(
            |state: &NodeState<S>, _params: serde_json::Value| -> Result<Vec<Dispute>, RpcError> {
                Ok(state.disputes.all())
            },
        ),
    );
    table.register(
        "sendDisputeOpen",
        method_fn(
            |state: &NodeState<S>, params: SendEventParams| -> Result<String, RpcError> {
                let bytes = decode_bytes(&params.data)?;
                let signed: SignedDisputeOpen =
                    json::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                let gossip_bytes =
                    wire::to_bytes(&signed).expect("SignedDisputeOpen always serializes");
                let id = state
                    .disputes
                    .apply_open(signed)
                    .map_err(|e| RpcError::Application(e.code()))?;
                crate::dispatch::originate(
                    state,
                    protocol::EVENT_OPENED,
                    protocol::OFS_SPEC,
                    Priority::Governance,
                    gossip_bytes,
                );
                Ok(id.as_str().to_string())
            },
        ),
    );
    table.register(
        "sendArbitratorJoin",
        method_fn(
            |state: &NodeState<S>, params: SendEventParams| -> Result<(), RpcError> {
                let bytes = decode_bytes(&params.data)?;
                let signed: SignedArbitratorJoin =
                    json::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                let gossip_bytes =
                    wire::to_bytes(&signed).expect("SignedArbitratorJoin always serializes");
                state
                    .disputes
                    .apply_arbitrator_join(signed)
                    .map_err(|e| RpcError::Application(e.code()))?;
                crate::dispatch::originate(
                    state,
                    protocol::EVENT_ARBITRATOR_JOINED,
                    protocol::OFS_SPEC,
                    Priority::Governance,
                    gossip_bytes,
                );
                Ok(())
            },
        ),
    );
    table.register(
        "sendVoteCommit",
        method_fn(
            |state: &NodeState<S>, params: SendEventParams| -> Result<(), RpcError> {
                let bytes = decode_bytes(&params.data)?;
                let signed: SignedVoteCommit =
                    json::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                let gossip_bytes =
                    wire::to_bytes(&signed).expect("SignedVoteCommit always serializes");
                state
                    .disputes
                    .apply_vote_commit(signed)
                    .map_err(|e| RpcError::Application(e.code()))?;
                crate::dispatch::originate(
                    state,
                    protocol::EVENT_VOTE_COMMITTED,
                    protocol::OFS_SPEC,
                    Priority::Governance,
                    gossip_bytes,
                );
                Ok(())
            },
        ),
    );
    table.register(
        "sendVoteReveal",
        method_fn(
            |state: &NodeState<S>, params: SendEventParams| -> Result<(), RpcError> {
                let bytes = decode_bytes(&params.data)?;
                let signed: SignedVoteReveal =
                    json::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                let gossip_bytes =
                    wire::to_bytes(&signed).expect("SignedVoteReveal always serializes");
                state
                    .disputes
                    .apply_vote_reveal(signed)
                    .map_err(|e| RpcError::Application(e.code()))?;
                crate::dispatch::originate(
                    state,
                    protocol::EVENT_VOTE_REVEALED,
                    protocol::OFS_SPEC,
                    Priority::Governance,
                    gossip_bytes,
                );
                Ok(())
            },
        ),
    );
}
