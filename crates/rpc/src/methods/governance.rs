//! Governance methods (OFS-4000).

use crate::dispatch::{IdParams, MethodTable, SendEventParams, decode_bytes, method_fn};
use crate::error::RpcError;
use crate::state::NodeState;
use openfiat_governance::events::{SignedProposalCreate, SignedVoteCast};
use openfiat_governance::{Proposal, ProposalId, protocol};
use openfiat_serialization::{json, wire};
use openfiat_storage::KvStore;
use openfiat_types::Priority;

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
                let gossip_bytes =
                    wire::to_bytes(&signed).expect("SignedProposalCreate always serializes");
                let id = state
                    .governance
                    .apply_create(signed)
                    .map_err(|e| RpcError::Application(e.code()))?;
                crate::dispatch::originate(
                    state,
                    protocol::EVENT_CREATED,
                    protocol::OFS_SPEC,
                    Priority::Governance,
                    gossip_bytes,
                );
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
                // Signature/authorship is checked now; the claimed
                // `weight` is not — this node never trusts a vote's own
                // self-report (see `VoteCast::weight`'s doc). Real
                // application (proposal existence, voting-window,
                // duplicate-vote checks, and finally recording the vote)
                // is deferred to `actor::poll_vote_verifications`, once
                // `stake_account` has been read and independently
                // confirmed on-chain.
                signed
                    .verify()
                    .map_err(|e| RpcError::Application(e.code()))?;
                let gossip_bytes =
                    wire::to_bytes(&signed).expect("SignedVoteCast always serializes");
                state.enqueue_vote_verification(
                    signed.vote.stake_account.clone(),
                    gossip_bytes.clone(),
                );
                crate::dispatch::originate(
                    state,
                    protocol::EVENT_VOTE_CAST,
                    protocol::OFS_SPEC,
                    Priority::Governance,
                    gossip_bytes,
                );
                Ok(())
            },
        ),
    );
}
