//! Governance methods (OFS-4000).

use crate::dispatch::{IdParams, MethodTable, SendEventParams, decode_bytes, method_fn};
use crate::error::RpcError;
use crate::state::NodeState;
use openfiat_governance::events::{SignedProposalCreate, SignedVoteCast};
use openfiat_governance::{Proposal, ProposalId};
use openfiat_serialization::json;
use openfiat_storage::KvStore;

pub fn register<S: KvStore + 'static>(table: &mut MethodTable<S>) {
    table.register(
        "getProposal",
        method_fn(
            |state: &NodeState<S>, params: IdParams| -> Result<Option<Proposal>, RpcError> {
                Ok(state.governance.get(&ProposalId::new(params.id)))
            },
        ),
    );
    table.register(
        "getProposals",
        method_fn(
            |state: &NodeState<S>, _params: serde_json::Value| -> Result<Vec<Proposal>, RpcError> {
                Ok(state.governance.all())
            },
        ),
    );
    table.register(
        "sendProposalCreate",
        method_fn(
            |state: &NodeState<S>, params: SendEventParams| -> Result<String, RpcError> {
                let bytes = decode_bytes(&params.data)?;
                let signed: SignedProposalCreate =
                    json::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                let id = state
                    .governance
                    .apply_create(signed)
                    .map_err(|e| RpcError::Application(e.code()))?;
                Ok(id.as_str().to_string())
            },
        ),
    );
    table.register(
        "sendVoteCast",
        method_fn(
            |state: &NodeState<S>, params: SendEventParams| -> Result<(), RpcError> {
                let bytes = decode_bytes(&params.data)?;
                let signed: SignedVoteCast =
                    json::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                state
                    .governance
                    .apply_vote(signed)
                    .map_err(|e| RpcError::Application(e.code()))
            },
        ),
    );
}
