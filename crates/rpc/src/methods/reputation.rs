//! Reputation methods (OFS-3000) — a pure read-side view, so there's no
//! `sendX` method here at all (see `openfiat_reputation`'s own crate doc
//! for why reputation has no signed event type of its own).
//!
//! `getReputation` returns the whole `ReputationProfile`, so every
//! dimension it gains — including §13's response counters and §14's
//! payment-discrepancy counters — appears here without a new method.
//! Rates are returned as their raw numerator and denominator rather than
//! a precomputed ratio, so a caller can aggregate across wallets and can
//! tell "no data yet" from "zero".

use crate::dispatch::{MethodTable, WalletParams, decode_peer_id, method_fn};
use crate::error::RpcError;
use crate::state::NodeState;
use openfiat_reputation::ReputationProfile;
use openfiat_storage::KvStore;

pub fn register<S: KvStore + 'static>(table: &mut MethodTable<S>) {
    table.register(
        "getReputation",
        method_fn(
            |state: &NodeState<S>, params: WalletParams| -> Result<ReputationProfile, RpcError> {
                Ok(state.reputation.profile(&decode_peer_id(&params.wallet)?))
            },
        ),
    );
}
