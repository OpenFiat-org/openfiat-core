//! Trade methods (OFS-2000) — a read-only join over reservations and
//! settlement, so there's no `sendX` method here at all.

use crate::dispatch::{IdParams, MethodTable, method_fn};
use crate::error::RpcError;
use crate::state::NodeState;
use openfiat_reservations::ReservationId;
use openfiat_storage::KvStore;
use openfiat_trade::Trade;

pub fn register<S: KvStore + 'static>(table: &mut MethodTable<S>) {
    table.register(
        "getTrade",
        method_fn(
            |state: &NodeState<S>, params: IdParams| -> Result<Option<Trade>, RpcError> {
                Ok(state.trades.get(&ReservationId::new(params.id)))
            },
        ),
    );
    table.register(
        "getTrades",
        method_fn(
            |state: &NodeState<S>, _params: serde_json::Value| -> Result<Vec<Trade>, RpcError> {
                Ok(state.trades.all())
            },
        ),
    );
}
