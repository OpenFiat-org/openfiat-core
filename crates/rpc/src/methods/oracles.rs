//! Oracle methods (OFS-7000).

use crate::dispatch::{IdParams, MethodTable, SendEventParams, decode_bytes, method_fn};
use crate::error::RpcError;
use crate::state::NodeState;
use openfiat_oracles::events::SignedOraclePublish;
use openfiat_oracles::store::ExchangeRateLookup;
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

/// Why a pair has no rate, which `getMedianExchangeRate` cannot say.
///
/// That method returns `Option<f64>` and collapses two different answers
/// into one `null`: a feed that has lapsed, and a corridor nobody prices
/// at all. `openfiat_oracles::store` distinguishes them — and its own doc
/// says the three-state lookup is "what anything pricing a trade should
/// read, rather than the `Option` above" — but only the flattened one was
/// ever exposed, so every client had to either guess or reimplement §11's
/// median itself against `getOracleRecords`. The app did the latter.
///
/// The distinction is not academic. Stale means a provider does publish
/// this pair and the feed will likely come back, so waiting is sensible.
/// NoData means nobody prices this corridor and waiting is pointless.
/// Neither is a number, and a caller must show neither as one.
///
/// `getMedianExchangeRate` stays, because clients depend on it and it is
/// the right shape when all a caller wants is a price or nothing.
#[derive(Debug, serde::Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum ExchangeRateView {
    /// A median over at least one unexpired record, good until the
    /// earliest expiry among the records that produced it.
    #[serde(rename_all = "camelCase")]
    Current { rate: f64, expires_at: Timestamp },
    /// Published, but every record has expired. OFS-7000 §12: expired data
    /// is not current data, however recently it lapsed.
    Stale,
    /// No provider publishes this pair.
    NoData,
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
        "getExchangeRate",
        method_fn(
            |state: &NodeState<S>,
             params: ExchangeRateParams|
             -> Result<ExchangeRateView, RpcError> {
                Ok(
                    match state
                        .oracles
                        .exchange_rate(&params.base, &params.quote, Timestamp::now())
                    {
                        ExchangeRateLookup::Current { rate, expires_at } => {
                            ExchangeRateView::Current { rate, expires_at }
                        }
                        ExchangeRateLookup::Stale => ExchangeRateView::Stale,
                        ExchangeRateLookup::NoData => ExchangeRateView::NoData,
                    },
                )
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

#[cfg(test)]
mod tests {
    use crate::dispatch::MethodTable;
    use crate::state::NodeState;
    use openfiat_storage::mem::MemoryStore;

    /// The whole reason `getExchangeRate` exists: two different answers
    /// that `getMedianExchangeRate` reports identically.
    #[test]
    fn an_unpriced_pair_says_so_rather_than_returning_nothing() {
        let state = NodeState::new_for_test(MemoryStore::new());
        let table: MethodTable<MemoryStore> = crate::methods::build_table();

        let answer = table
            .dispatch(
                &state,
                "getExchangeRate",
                serde_json::json!({ "base": "USDC", "quote": "KES" }),
            )
            .expect("a pair nobody publishes is an answer, not an error");
        assert_eq!(answer["status"], "noData");
        assert!(
            answer.get("rate").is_none(),
            "an absent rate must be absent, not zero — a caller that read \
             a zero would price a trade at nothing"
        );

        // The same question through the older method. `null` is all it can
        // say, which is the flattening this test exists to document.
        let flattened = table
            .dispatch(
                &state,
                "getMedianExchangeRate",
                serde_json::json!({ "base": "USDC", "quote": "KES" }),
            )
            .expect("the older method answers too");
        assert!(flattened.is_null());
    }
}
