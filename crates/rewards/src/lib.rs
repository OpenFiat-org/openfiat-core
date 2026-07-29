//! `openfiat-rewards` — what each node operator is owed for an epoch.
//!
//! Related specification: OFS-4100 §9 (Reward Distribution).
//!
//! # Why this crate exists
//!
//! `openfiat-staking`'s `distribute_reward` instruction has been deployed
//! for a long time and has never been called, because nothing computed an
//! amount to pass it. Its own doc comment says it "only trusts and
//! records the already-decided amount" — this crate is the part that
//! decides.
//!
//! # What it deliberately does not do
//!
//! It does not sign or submit anything. A node in this workspace never
//! builds a Solana transaction: `crates/chain`'s [`ChainClient`] relays
//! *already-signed* bytes that arrived from a client, and the node holds
//! no key with authority over on-chain funds. Making the reward cranker
//! the first exception would put the `rewards_authority` private key on a
//! gossip-facing daemon and give the node an on-chain spending power it
//! has never had.
//!
//! So the split follows the one this workspace already uses everywhere
//! else — off-chain decides, a signed client transaction executes:
//!
//! 1. Any node computes a [`RewardSchedule`] for a completed epoch.
//! 2. The `rewards_authority` holder builds and signs one
//!    `distribute_reward` per entry, exactly as the SDKs already build
//!    every other mutation, and submits it through the ordinary
//!    `sendTransaction` relay.
//!
//! The useful property this buys is that step 1 is reproducible. The
//! schedule is a pure function of the epoch's observations, the on-chain
//! stakes and the parameters, so anyone can recompute it and compare. An
//! authority that pays a different number can be caught; one that
//! computed the schedule privately could not be.
//!
//! # What it cannot honestly measure
//!
//! [`liveness`] documents this at length, and the summary is that
//! connectivity is a lower bound rather than a proof — a gossip-only node
//! can re-announce a blockhash it heard elsewhere under its own
//! signature, and nothing in the envelope distinguishes that from a real
//! observation. OFS-4100 §9.2 already records that a fully
//! manipulation-resistant measure is unsolved; this crate does not
//! pretend to have solved it.
//!
//! # Still blocked elsewhere
//!
//! Computing and crediting a reward is not the same as paying one.
//! `claim_rewards` transfers from the staking program's rewards vault,
//! no instruction funds that vault, and its live balance is zero. Until
//! that is fixed a node can be credited `pending_rewards` and still not
//! be able to claim.
//!
//! [`ChainClient`]: https://docs.rs/openfiat-chain

pub mod liveness;
pub mod params;
pub mod schedule;

pub use liveness::{LivenessLedger, PeerLiveness};
pub use params::{InvalidParams, RewardParams};
pub use schedule::{Eligibility, PaidEpochs, RewardEntry, RewardSchedule, compute, payable_epochs};

/// Crate version, re-exported for diagnostics and `openfiat-node --version`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_a_version() {
        assert!(!version().is_empty());
    }
}
