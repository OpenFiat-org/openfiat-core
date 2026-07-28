//! Oracle methods (OFS-7000).

use crate::dispatch::{IdParams, MethodTable, SendEventParams, decode_bytes, method_fn};
use crate::error::RpcError;
use crate::state::NodeState;
use openfiat_oracles::events::SignedOraclePublish;
use openfiat_oracles::{OracleId, OracleRecord, protocol};
use openfiat_serialization::{json, wire};
use openfiat_storage::KvStore;
use openfiat_types::{Priority, Timestamp};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct ExchangeRateParams {
    pub base: String,
    pub quote: String,
}

pub fn register<S: KvStore + 'static>(table: &mut MethodTable<S>) {
    table.register(
        "getOracleRecord",
        method_fn(
            |state: &NodeState<S>, params: IdParams| -> Result<Option<OracleRecord>, RpcError> {
                Ok(state.oracles.get(&OracleId::new(params.id)))
            },
        ),
    );
    table.register(
        "getOracleRecords",
        method_fn(
            |state: &NodeState<S>,
             _params: serde_json::Value|
             -> Result<Vec<OracleRecord>, RpcError> { Ok(state.oracles.all()) },
        ),
    );
    table.register(
        "getMedianExchangeRate",
        method_fn(
            |state: &NodeState<S>, params: ExchangeRateParams| -> Result<Option<f64>, RpcError> {
                Ok(state.oracles.median_exchange_rate(
                    &params.base,
                    &params.quote,
                    Timestamp::now(),
                ))
            },
        ),
    );
    table.register(
        "sendOraclePublish",
        method_fn(
            |state: &NodeState<S>, params: SendEventParams| -> Result<String, RpcError> {
                let bytes = decode_bytes(&params.data)?;
                let signed: SignedOraclePublish =
                    json::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                let gossip_bytes =
                    wire::to_bytes(&signed).expect("SignedOraclePublish always serializes");
                let id = state
                    .oracles
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
