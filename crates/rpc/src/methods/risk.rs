//! Risk intelligence methods (OFS-7100).

use crate::dispatch::{
    MethodTable, SendEventParams, WalletParams, decode_bytes, decode_peer_id, method_fn,
};
use crate::error::RpcError;
use crate::state::NodeState;
use openfiat_risk::events::SignedRiskPublish;
use openfiat_risk::{RiskOutcome, RiskRecord, ScreeningResult, protocol};
use openfiat_serialization::{json, wire};
use openfiat_storage::KvStore;
use openfiat_types::{Priority, Timestamp};

pub fn register<S: KvStore + 'static>(table: &mut MethodTable<S>) {
    table.register(
        "getRiskRecordsByWallet",
        method_fn(
            |state: &NodeState<S>, params: WalletParams| -> Result<Vec<RiskRecord>, RpcError> {
                Ok(state.risk.for_wallet(&decode_peer_id(&params.wallet)?))
            },
        ),
    );
    table.register(
        "getWalletScreening",
        method_fn(
            |state: &NodeState<S>, params: WalletParams| -> Result<ScreeningResult, RpcError> {
                Ok(state
                    .risk
                    .screen(&decode_peer_id(&params.wallet)?, Timestamp::now()))
            },
        ),
    );
    table.register(
        "sendRiskPublish",
        method_fn(
            |state: &NodeState<S>, params: SendEventParams| -> Result<(), RpcError> {
                let bytes = decode_bytes(&params.data)?;
                let signed: SignedRiskPublish =
                    json::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                let gossip_bytes =
                    wire::to_bytes(&signed).expect("SignedRiskPublish always serializes");
                let event_type = match signed.publish.outcome {
                    RiskOutcome::Flagged => protocol::EVENT_FLAGGED,
                    RiskOutcome::Cleared => protocol::EVENT_CLEARED,
                };
                state
                    .risk
                    .apply_publish(signed)
                    .map_err(|e| RpcError::Application(e.code()))?;
                crate::dispatch::originate(
                    state,
                    event_type,
                    protocol::OFS_SPEC,
                    Priority::Reputation,
                    gossip_bytes,
                );
                Ok(())
            },
        ),
    );
}
