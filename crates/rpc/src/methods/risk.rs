//! Risk intelligence methods (OFS-7100).

use crate::dispatch::{
    MethodTable, SendEventParams, WalletParams, decode_bytes, decode_peer_id, method_fn,
};
use crate::error::RpcError;
use crate::state::NodeState;
use openfiat_risk::events::SignedRiskPublish;
use openfiat_risk::{RiskRecord, ScreeningResult};
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::Timestamp;

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
                    wire::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                state
                    .risk
                    .apply_publish(signed)
                    .map_err(|e| RpcError::Application(e.code()))
                    .map(|_| ())
            },
        ),
    );
}
