//! Governance methods (OFS-4000).

use crate::dispatch::{IdParams, MethodTable, SendEventParams, decode_bytes, method_fn};
use crate::error::RpcError;
use crate::state::NodeState;
use openfiat_governance::events::{SignedProposalCreate, SignedVoteCast};
use openfiat_governance::onchain::onchain_proposal_address;
use openfiat_governance::{ChainAgreement, Proposal, ProposalId, ProposalStatus, protocol};
use openfiat_serialization::{json, wire};
use openfiat_storage::KvStore;
use openfiat_types::Priority;

/// Everything a client needs to set an off-chain proposal beside the
/// chain's record of it.
///
/// # Why this method exists
///
/// Off-chain proposals and the governance program's `Proposal` accounts
/// were entirely uncorrelated, so an interface showing "the" proposal was
/// showing one of two records and implying the other — with no way to
/// notice when the chain disagreed. This is the join, reported rather
/// than hidden: the id and address of the chain's half, the digest the
/// chain stores for this proposal, and an explicit verdict on whether the
/// two records actually name each other.
///
/// # What `agreement` is, and is not
///
/// It reflects what this node has adopted, not a live account read: these
/// handlers are synchronous and hold no chain client. So `agreement` will
/// read `ClaimNotReciprocated` on a node that has not yet fetched the
/// account — correct, if unsatisfying, since an unfetched claim is
/// precisely one this node cannot corroborate. A client wanting the live
/// answer fetches `onchain_proposal_address` itself; the point of
/// returning it is that it does not have to derive it.
#[derive(serde::Serialize)]
pub struct ProposalChainLink {
    /// The on-chain `Proposal` id this proposal claims, from the signed
    /// `ProposalCreate` event. `null` for a proposal that never went on
    /// chain.
    pub onchain_proposal_id: Option<u64>,
    /// That proposal's account address, derived so a client does not have
    /// to. `null` whenever `onchain_proposal_id` is.
    pub onchain_proposal_address: Option<String>,
    /// The digest the on-chain proposal must carry for the link to hold —
    /// and exactly what to pass to the program's `link_offchain_proposal`
    /// when creating the other half.
    pub offchain_id_hash: String,
    pub governance_program: String,
    /// This node's off-chain status, so the caller can see what the
    /// verdict below is comparing against.
    pub status: ProposalStatus,
    pub agreement: ChainAgreement,
}

/// Lowercase hex, so a digest is something a caller can paste rather than
/// a 32-element array of numbers.
fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

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
        "getProposalChainLink",
        method_fn(
            |state: &NodeState<S>, params: IdParams| -> Result<ProposalChainLink, RpcError> {
                let id = ProposalId::new(params.id);
                let proposal = state
                    .governance
                    .get(&id)
                    .ok_or_else(|| RpcError::InvalidParams("no such proposal".into()))?;

                // `chain_view` with no on-chain record: this handler is
                // synchronous and holds no `ChainClient`, so it reports
                // what this node has *already* adopted rather than
                // fetching the account itself. That is the honest scope —
                // and it is why `onchain_proposal_address` is returned:
                // a client that wants the live account can fetch it
                // itself, from an address it did not have to derive.
                let view = state
                    .governance
                    .chain_view(&id, None)
                    .expect("the proposal was just read");

                Ok(ProposalChainLink {
                    onchain_proposal_id: proposal.onchain_proposal_id,
                    onchain_proposal_address: proposal
                        .onchain_proposal_id
                        .map(onchain_proposal_address),
                    offchain_id_hash: hex(&openfiat_governance::offchain_id_hash(&id)),
                    governance_program: openfiat_chain::PROGRAM_IDS.governance.to_string(),
                    status: view.offchain.status,
                    agreement: view.agreement,
                })
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
