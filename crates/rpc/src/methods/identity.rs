//! Identity claim methods (OFS-5000).

use crate::dispatch::{
    IdParams, MethodTable, SendEventParams, WalletParams, decode_bytes, decode_peer_id, method_fn,
};
use crate::error::RpcError;
use crate::state::NodeState;
use openfiat_identity::events::SignedClaimPublish;
use openfiat_identity::{Claim, ClaimId, protocol};
use openfiat_serialization::{json, wire};
use openfiat_storage::KvStore;
use openfiat_types::Priority;

pub fn register<S: KvStore + 'static>(table: &mut MethodTable<S>) {
    table.register(
        "getIdentityClaim",
        method_fn(
            |state: &NodeState<S>, params: IdParams| -> Result<Option<Claim>, RpcError> {
                Ok(state.identity.get(&ClaimId::new(params.id)))
            },
        ),
    );
    table.register(
        "getIdentityClaimsByWallet",
        method_fn(
            |state: &NodeState<S>, params: WalletParams| -> Result<Vec<Claim>, RpcError> {
                Ok(state
                    .identity
                    .find_by_wallet(&decode_peer_id(&params.wallet)?))
            },
        ),
    );
    table.register(
        "sendClaimPublish",
        method_fn(
            |state: &NodeState<S>, params: SendEventParams| -> Result<String, RpcError> {
                let bytes = decode_bytes(&params.data)?;
                let signed: SignedClaimPublish =
                    json::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                let gossip_bytes =
                    wire::to_bytes(&signed).expect("SignedClaimPublish always serializes");
                let id = state
                    .identity
                    .apply_publish(signed)
                    .map_err(|e| RpcError::Application(e.code()))?;
                crate::dispatch::originate(
                    state,
                    protocol::EVENT_CREATED,
                    protocol::OFS_SPEC,
                    Priority::Reputation,
                    gossip_bytes,
                );
                Ok(id.as_str().to_string())
            },
        ),
    );
}
