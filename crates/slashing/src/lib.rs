//! `openfiat-slashing` — which arbitrators must be slashed for a resolved
//! dispute, and which must not.
//!
//! Related specifications: OFS-2400 §328 (the rule), OFS-4100 §4 (the
//! enumerated triggers and the rate), OFS-4200 §1 (why this is a relay
//! rather than a CPI).
//!
//! # Why this crate exists
//!
//! `openfiat-staking`'s `slash` instruction has existed since the staking
//! program shipped and nothing has ever called it. Voting against
//! consensus has therefore been free, which leaves the arbitration
//! incentive one-sided: OFS-2400 §328 promises that "arbitrators whose
//! revealed vote falls outside consensus may incur a partial, moderate
//! stake slash", and the reward half of that sentence is now implemented
//! while the penalty half was not.
//!
//! It is also the residual risk left by the Sybil fix. Seat-squatting now
//! costs real stake, but an attacker holding that stake can still occupy
//! every seat and force the terminal even split — losing the buyer half
//! the trade. That attack is only *expensive* once the squatters, who by
//! construction reveal against whatever consensus forms, are slashed.
//!
//! # Why it is a relay and not a CPI
//!
//! OFS-4200 §1 deliberately forbids `escrow` calling `staking` directly,
//! so that a bug in one program's CPI-calling code cannot corrupt the
//! other's state. The dispute tally therefore lands on-chain in
//! `DisputeCase`, and a separate signed instruction applies the
//! consequence. This crate is the part that reads the former and decides
//! the latter.
//!
//! # What it deliberately does not do
//!
//! It does not sign or submit anything, for the same reason
//! `openfiat-rewards` does not: a node
//! in this workspace never builds a Solana transaction, `crates/chain`
//! relays only already-signed bytes, and the node holds no key with
//! authority over funds. Putting the `slashing_authority` key on a
//! gossip-facing daemon would hand a node the power to confiscate stake.
//!
//! So the split is the one used everywhere else — off-chain decides, a
//! signed client transaction executes:
//!
//! 1. Anyone reads a resolved [`ResolvedCase`] off-chain and computes a
//!    [`SlashSchedule`].
//! 2. The `slashing_authority` holder submits one `slash` per entry.
//!
//! The property this buys is that step 1 is reproducible. The schedule is
//! a pure function of immutable on-chain state, so any observer can
//! recompute it and compare. An authority that slashes someone the tally
//! does not condemn can be caught; one that decided privately could not.

pub mod verdict;

pub use verdict::{
    MISCONDUCT_OUTSIDE_CONSENSUS, ResolvedCase, SlashEntry, SlashSchedule, SlashedCases, compute,
};

/// Crate version, re-exported for diagnostics.
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
