//! Reward observation methods (OFS-4100 §9).
//!
//! Read-only. This node publishes what it observed; it never signs or
//! submits a `distribute_reward` — see `openfiat_rewards`' crate doc for
//! why that stays a client action.
//!
//! Publishing the observations is the point of §9.4: a schedule anyone
//! can recompute is a schedule whose author can be checked. Two nodes
//! that disagree here are visible, where two nodes each computing
//! privately would not be.
//!
//! There is deliberately no `getRewardSchedule`. Turning observations
//! into amounts needs each candidate's on-chain stake, and this dispatch
//! is synchronous — the same constraint that pushed governance's
//! vote-weight check onto `actor::poll_vote_verifications`. A method that
//! answered anyway would return an empty schedule every time while
//! looking like a working endpoint. Callers with stakes to hand use
//! `openfiat_rewards::compute` directly; wiring the async stake read is
//! the natural next step and is not faked in the interim.

use crate::dispatch::{MethodTable, method_fn};
use crate::error::RpcError;
use crate::state::NodeState;
use openfiat_storage::KvStore;
use openfiat_types::Timestamp;

#[derive(serde::Deserialize)]
pub struct EpochParams {
    /// Omitted means "the most recently completed epoch", which is the
    /// only one worth asking about — the in-flight epoch's answer would
    /// change under the caller.
    #[serde(default)]
    pub epoch: Option<u64>,
}

#[derive(serde::Serialize)]
pub struct ObservedPeer {
    /// Hex, so a peer id survives JSON without a base58 dependency here.
    pub peer: String,
    pub availability_bps: u64,
    pub connectivity_bps: u64,
    pub announced_blockhash: bool,
}

#[derive(serde::Serialize)]
pub struct EpochObservations {
    pub epoch: u64,
    pub epoch_start_millis: u64,
    pub epoch_end_millis: u64,
    /// Ordered by peer, so two nodes' answers are directly comparable.
    pub peers: Vec<ObservedPeer>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The epoch to answer for: the caller's, or the last completed one.
fn resolve_epoch<S: KvStore>(state: &NodeState<S>, requested: Option<u64>) -> u64 {
    requested.unwrap_or_else(|| {
        state
            .reward_params
            .epoch_index(Timestamp::now())
            .saturating_sub(1)
    })
}

pub fn register<S: KvStore + 'static>(table: &mut MethodTable<S>) {
    table.register(
        "getRewardObservations",
        method_fn(
            |state: &NodeState<S>, params: EpochParams| -> Result<EpochObservations, RpcError> {
                let epoch = resolve_epoch(state, params.epoch);
                let (start, end) = state.reward_params.epoch_bounds(epoch);
                let observed = state.reward_observations.borrow().epoch(epoch);

                let mut peers: Vec<ObservedPeer> = observed
                    .iter()
                    .map(|(peer, live)| ObservedPeer {
                        peer: hex(peer.as_bytes()),
                        availability_bps: live.availability_bps(&state.reward_params),
                        connectivity_bps: live.connectivity_bps(&state.reward_params),
                        announced_blockhash: live.announced_blockhash,
                    })
                    .collect();
                peers.sort_by(|a, b| a.peer.cmp(&b.peer));

                Ok(EpochObservations {
                    epoch,
                    epoch_start_millis: start,
                    epoch_end_millis: end,
                    peers,
                })
            },
        ),
    );
}
