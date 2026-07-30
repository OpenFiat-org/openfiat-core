//! Dispute methods (OFS-2400).

use crate::dispatch::{IdParams, MethodTable, SendEventParams, decode_bytes, method_fn};
use crate::error::RpcError;
use crate::methods::redaction::PublicDispute;
use crate::methods::wallet_auth::{WalletProof, verify_wallet};
use crate::state::NodeState;
use openfiat_disputes::events::{
    SignedArbitratorJoin, SignedDisputeOpen, SignedVoteCommit, SignedVoteReveal,
};
use openfiat_disputes::{Dispute, DisputeId, protocol};
use openfiat_serialization::{json, wire};
use openfiat_storage::KvStore;
use openfiat_types::Priority;

/// Domain separator for `getMyDisputes`.
pub const CHALLENGE_DOMAIN: &str = "openfiat-my-disputes";

pub fn register<S: KvStore + 'static>(table: &mut MethodTable<S>) {
    table.register(
        // Redacted. A dispute is the record where knowing who fell out
        // with whom is most obviously worth misusing, and it additionally
        // carries free-text `reason` and the arbitrator-to-vote pairing.
        "getDispute",
        method_fn(
            |state: &NodeState<S>, params: IdParams| -> Result<Option<PublicDispute>, RpcError> {
                Ok(state
                    .disputes
                    .get(&DisputeId::new(params.id))
                    .map(PublicDispute::from))
            },
        ),
    );
    table.register(
        "getDisputes",
        method_fn(
            |state: &NodeState<S>,
             _params: serde_json::Value|
             -> Result<Vec<PublicDispute>, RpcError> {
                Ok(state
                    .disputes
                    .all()
                    .into_iter()
                    .map(PublicDispute::from)
                    .collect())
            },
        ),
    );
    table.register(
        "getMyDisputes",
        method_fn(
            |state: &NodeState<S>, params: WalletProof| -> Result<Vec<Dispute>, RpcError> {
                let wallet = verify_wallet(state, &params, CHALLENGE_DOMAIN)?;
                // A seated arbitrator needs the whole case — that is
                // their job — and a party needs their own. Nobody else
                // gets either.
                Ok(state
                    .disputes
                    .all()
                    .into_iter()
                    .filter(|d| {
                        d.buyer == wallet || d.seller == wallet || d.arbitrators.contains(&wallet)
                    })
                    .collect())
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
