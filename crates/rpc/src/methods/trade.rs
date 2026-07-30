//! Trade methods (OFS-2000) — a read-only join over reservations and
//! settlement, so there's no `sendX` method here at all.

use crate::dispatch::{IdParams, MethodTable, method_fn};
use crate::error::RpcError;
use crate::methods::redaction::PublicTrade;
use crate::methods::wallet_auth::{WalletProof, verify_wallet};
use crate::state::NodeState;
use openfiat_reservations::ReservationId;
use openfiat_storage::KvStore;
use openfiat_trade::Trade;

/// Domain separator for `getMyTrades`.
pub const CHALLENGE_DOMAIN: &str = "openfiat-my-trades";

pub fn register<S: KvStore + 'static>(table: &mut MethodTable<S>) {
    table.register(
        // Redacted like the reads it joins. This method was the way
        // around them: a trade embeds both records whole, so leaving it
        // open would have left the trade graph one method along from
        // where it was closed.
        "getTrade",
        method_fn(
            |state: &NodeState<S>, params: IdParams| -> Result<Option<PublicTrade>, RpcError> {
                Ok(state
                    .trades
                    .get(&ReservationId::new(params.id))
                    .map(PublicTrade::from))
            },
        ),
    );
    table.register(
        "getTrades",
        method_fn(
            |state: &NodeState<S>,
             _params: serde_json::Value|
             -> Result<Vec<PublicTrade>, RpcError> {
                Ok(state
                    .trades
                    .all()
                    .into_iter()
                    .map(PublicTrade::from)
                    .collect())
            },
        ),
    );
    table.register(
        "getMyTrades",
        method_fn(
            |state: &NodeState<S>, params: WalletProof| -> Result<Vec<Trade>, RpcError> {
                let wallet = verify_wallet(state, &params, CHALLENGE_DOMAIN)?;
                // A party to the reservation, or to the settlement it
                // became. Both are checked because a trade exists before
                // a settlement does, and the buyer is the only party to
                // it at that point.
                Ok(state
                    .trades
                    .all()
                    .into_iter()
                    .filter(|trade| {
                        trade.reservation.requester == wallet
                            || trade
                                .settlement
                                .as_ref()
                                .is_some_and(|s| s.buyer == wallet || s.seller == wallet)
                    })
                    .collect())
            },
        ),
    );
}
