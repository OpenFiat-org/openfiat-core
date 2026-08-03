//! The arbitrator-pool floor, and why a terminal even split happened.
//!
//! OFS-4100 Annex A's finding is that two individually-sound parameters
//! compose into an unstated constraint on the arbitrator pool. Every round
//! but the last can retire a full bench of silent seat-holders
//! ([`MAX_BARRED_ARBITRATORS`]), and the last round still needs
//! [`MIN_ARBITRATORS`] counted reveals — so a case is only decidable at all
//! if the eligible pool holds [`MIN_DECIDABLE_ARBITRATOR_POOL`] wallets.
//! Below that the final round cannot reach quorum *structurally*, the case
//! lands on the terminal even split, and the even split is precisely what a
//! losing party wants.
//!
//! This module holds the two things that follow from that: a classification
//! of *why* a case ended on the split, and the arithmetic deciding whether
//! another round is worth opening at all.
//!
//! # The split is the same either way
//!
//! Nothing here changes a payout. A case that would have bounced twice more
//! and split still splits, for the same amounts, to the same parties. What
//! changes is that the protocol stops pretending a decision was attempted
//! when it provably could not be, and that an operator can tell the
//! difference between "the arbitrators disagreed" and "there were not enough
//! arbitrators left to ask". Those look identical today: three indecisive
//! rounds and a split.

use anchor_lang::prelude::*;

use crate::state::{MAX_ARBITRATORS, MAX_BARRED_ARBITRATORS, MIN_ARBITRATORS};

/// The smallest eligible arbitrator pool on which a dispute case can be
/// decided at all.
///
/// Not a policy number — it is read straight off the two parameters that
/// impose it. A case may retire up to [`MAX_BARRED_ARBITRATORS`] wallets for
/// staying silent, and the round that ends the case still needs
/// [`MIN_ARBITRATORS`] counted reveals from wallets that were never retired.
/// With `MAX_ARBITRATORS = 7`, `MAX_DISPUTE_ROUNDS = 3` and
/// `MIN_ARBITRATORS = 3` that is `14 + 3 = 17`.
///
/// In practice the real requirement is higher, because qualifying for a seat
/// is not the same as taking one and revealing on time. This is the floor
/// below which the case is *structurally* undecidable, not the pool size at
/// which arbitration works comfortably.
pub const MIN_DECIDABLE_ARBITRATOR_POOL: u32 =
    MIN_ARBITRATORS as u32 + MAX_BARRED_ARBITRATORS as u32;

/// Ties the constant above to the parameters it is derived from, so a change
/// to the seat count or the round budget cannot silently leave the floor
/// describing a protocol that no longer exists.
const _: () = assert!(
    MIN_DECIDABLE_ARBITRATOR_POOL as usize
        == MIN_ARBITRATORS + MAX_ARBITRATORS * (crate::constants::MAX_DISPUTE_ROUNDS as usize - 1),
    "MIN_DECIDABLE_ARBITRATOR_POOL no longer matches the seat count and round budget it is \
     derived from — revisit OFS-4100 Annex A before changing either"
);

/// Why a dispute case ended on the terminal even split rather than on a
/// verdict.
///
/// Recorded on the case and emitted as
/// [`DisputeTerminalSplit`](crate::events::DisputeTerminalSplit). The split
/// itself is unchanged by which of these applies; the point is that they are
/// different failures with different operator responses, and until now they
/// were indistinguishable from outside.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace, Debug)]
pub enum TerminalSplitReason {
    /// Enough arbitrators served and enough of them revealed, and the
    /// weighted tally still came out tied — every round, to the budget.
    ///
    /// This is the only one of the four that means arbitration worked and
    /// the case was genuinely undecidable. Nothing to act on.
    NoConsensus,
    /// The final round seated at least [`MIN_ARBITRATORS`] arbitrators, but
    /// fewer than that many reveals were counted.
    ///
    /// The seat-squatting shape: seats were taken and abandoned. The barring
    /// rule is what answers this within a case, so seeing it at the round
    /// budget means the attacker had enough wallets to outlast the bar.
    QuorumNotReached,
    /// The final round could not even seat [`MIN_ARBITRATORS`] arbitrators.
    ///
    /// Quorum was unreachable before a single vote was cast, so the round
    /// was never a decision that failed — it was a decision that could not
    /// be attempted. Derived entirely from the case's own bookkeeping, with
    /// no reliance on a published pool size, which is why it is reported
    /// even on deployments where governance has published nothing.
    RoundUnstaffed,
    /// The case stopped short of its round budget because the eligible pool
    /// could not staff another round.
    ///
    /// The only variant that reflects the early exit rather than the round
    /// budget running out, and the only one that depends on
    /// [`crate::state::ArbitrationPolicy`] having been published — see
    /// [`PoolFloor::next_round_is_staffable`].
    PoolExhausted,
}

/// Everything the pool-floor decision is made from, gathered at the moment a
/// round closes without deciding.
///
/// A plain struct of counts rather than a borrow of the case, so the whole
/// decision is a pure function over numbers and can be tested without a
/// validator, an account or a runtime.
#[derive(Clone, Copy, Debug)]
pub struct PoolFloor {
    /// Wallets barred from this case, counted *after* the round that just
    /// closed has retired its silent seats. This is the demand side of the
    /// floor and the program knows it exactly.
    pub barred: u32,
    /// How many times a seat has been taken across every round of this case.
    ///
    /// An **over**-count of the distinct wallets involved, because a wallet
    /// that serves honestly in two rounds is counted twice. That direction is
    /// deliberate: this figure is only ever used to raise the pool estimate,
    /// so over-counting makes the floor harder to trip, never easier. See
    /// [`Self::evidenced_pool`].
    pub seats_taken_total: u32,
    /// Seats filled in the round that just closed.
    pub seats_this_round: u32,
    /// Reveals the tally actually counted in the round that just closed —
    /// the same figure [`MIN_ARBITRATORS`] is checked against, so
    /// zero-weight reveals are already excluded.
    pub counted_reveals: u32,
    /// Governance's published count of wallets eligible to arbitrate, from
    /// [`crate::state::ArbitrationPolicy`]. **Zero means unpublished**, and
    /// disables the floor completely.
    pub published_pool: u32,
}

impl PoolFloor {
    /// How large the eligible pool must be for the *next* round to have any
    /// chance of deciding: the quorum floor, plus every wallet this case has
    /// already retired.
    ///
    /// Exactly Annex A's `eligible pool >= MIN_ARBITRATORS + barred so far`.
    pub fn required_for_next_round(&self) -> u32 {
        MIN_ARBITRATORS as u32 + self.barred
    }

    /// The largest pool size the program can justify believing in.
    ///
    /// Two independent sources, and the larger wins:
    ///
    /// - what governance published, which may be stale in either direction;
    /// - what this case has *witnessed* — wallets that actually took a seat
    ///   here are proof the pool is at least that big, and no published
    ///   number can argue them away.
    ///
    /// Taking the maximum is what stops a stale-low published figure from
    /// ending a case that is visibly attracting arbitrators. It is a
    /// one-sided guard and honestly so: the witnessed count is a lower bound
    /// on the pool, so it can only ever raise the estimate. Nothing on chain
    /// can put an upper bound on the pool, which is why the floor is
    /// disabled entirely until governance publishes one.
    pub fn evidenced_pool(&self) -> u32 {
        self.published_pool.max(self.seats_taken_total)
    }

    /// Whether opening another round is worth doing.
    ///
    /// Returns `true` — "go ahead" — whenever the program has no grounds to
    /// say otherwise, which includes the whole of the default deployment
    /// where `published_pool` is zero. That default is the important one: a
    /// wrong pool estimate that *blocks* a decidable case is worse than the
    /// bug this floor exists to close, because it hands the griefing party
    /// their even split sooner and for free. So the floor refuses a round
    /// only on a positive statement from governance that the pool is too
    /// small, corroborated against what the case itself has seen.
    pub fn next_round_is_staffable(&self) -> bool {
        if self.published_pool == 0 {
            return true;
        }
        self.evidenced_pool() >= self.required_for_next_round()
    }

    /// Why the case ended, given that it ended on the terminal split with
    /// its round budget spent.
    ///
    /// Depends on nothing but the case's own arrays, so it is reported on
    /// every deployment whether or not a pool size has ever been published.
    pub fn exhausted_rounds_reason(&self) -> TerminalSplitReason {
        if self.seats_this_round < MIN_ARBITRATORS as u32 {
            TerminalSplitReason::RoundUnstaffed
        } else if self.counted_reveals < MIN_ARBITRATORS as u32 {
            TerminalSplitReason::QuorumNotReached
        } else {
            TerminalSplitReason::NoConsensus
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::MAX_DISPUTE_ROUNDS;

    /// A case that has run cleanly so far, parameterised by the two figures
    /// each test actually cares about.
    fn floor(barred: u32, published_pool: u32) -> PoolFloor {
        PoolFloor {
            barred,
            seats_taken_total: barred,
            seats_this_round: 0,
            counted_reveals: 0,
            published_pool,
        }
    }

    #[test]
    fn the_floor_is_the_one_annex_a_derives() {
        assert_eq!(MIN_DECIDABLE_ARBITRATOR_POOL, 17);
        assert_eq!(MAX_BARRED_ARBITRATORS, 14);
        assert_eq!(MAX_DISPUTE_ROUNDS, 3);
    }

    #[test]
    fn an_unpublished_pool_never_refuses_a_round() {
        // Every value of `barred` a case can reach, including the one that
        // makes the next round provably hopeless. With nothing published the
        // program has no standing to say so, and must not guess.
        for barred in 0..=MAX_BARRED_ARBITRATORS as u32 {
            assert!(
                floor(barred, 0).next_round_is_staffable(),
                "an unpublished pool must never block a round (barred={barred})"
            );
        }
    }

    #[test]
    fn a_published_pool_at_the_floor_still_staffs_the_last_round() {
        let f = PoolFloor {
            barred: MAX_BARRED_ARBITRATORS as u32,
            seats_taken_total: MAX_BARRED_ARBITRATORS as u32,
            seats_this_round: 7,
            counted_reveals: 0,
            published_pool: MIN_DECIDABLE_ARBITRATOR_POOL,
        };
        assert_eq!(f.required_for_next_round(), MIN_DECIDABLE_ARBITRATOR_POOL);
        assert!(f.next_round_is_staffable());
    }

    #[test]
    fn one_wallet_below_the_floor_refuses_the_last_round() {
        let f = floor(
            MAX_BARRED_ARBITRATORS as u32,
            MIN_DECIDABLE_ARBITRATOR_POOL - 1,
        );
        assert!(!f.next_round_is_staffable());
    }

    /// The whole point of the maximum in [`PoolFloor::evidenced_pool`]: a
    /// published figure the case has already outgrown must not end it.
    #[test]
    fn witnessed_participation_overrides_a_stale_low_published_figure() {
        let f = PoolFloor {
            barred: 7,
            // Twenty wallets have taken a seat here, so the pool is at least
            // twenty whatever governance last wrote down.
            seats_taken_total: 20,
            seats_this_round: 7,
            counted_reveals: 0,
            published_pool: 4,
        };
        assert_eq!(f.required_for_next_round(), 10);
        assert_eq!(f.evidenced_pool(), 20);
        assert!(f.next_round_is_staffable());
    }

    /// A pool that cannot reach quorum even with nothing barred. The case is
    /// stopped at the end of its very first round rather than bounced twice
    /// more for form's sake.
    #[test]
    fn a_pool_below_the_quorum_floor_stops_the_first_round() {
        let f = floor(0, MIN_ARBITRATORS as u32 - 1);
        assert_eq!(f.required_for_next_round(), MIN_ARBITRATORS as u32);
        assert!(!f.next_round_is_staffable());
        assert!(floor(0, MIN_ARBITRATORS as u32).next_round_is_staffable());
    }

    #[test]
    fn an_unstaffed_final_round_is_not_reported_as_disagreement() {
        let f = PoolFloor {
            barred: 14,
            seats_taken_total: 16,
            seats_this_round: 2,
            counted_reveals: 2,
            published_pool: 0,
        };
        assert_eq!(
            f.exhausted_rounds_reason(),
            TerminalSplitReason::RoundUnstaffed
        );
    }

    #[test]
    fn seats_taken_and_abandoned_read_as_a_missing_quorum() {
        let f = PoolFloor {
            barred: 14,
            seats_taken_total: 21,
            seats_this_round: 7,
            counted_reveals: 2,
            published_pool: 0,
        };
        assert_eq!(
            f.exhausted_rounds_reason(),
            TerminalSplitReason::QuorumNotReached
        );
    }

    #[test]
    fn a_full_bench_that_simply_tied_reads_as_no_consensus() {
        let f = PoolFloor {
            barred: 0,
            seats_taken_total: 21,
            seats_this_round: 6,
            counted_reveals: 6,
            published_pool: 0,
        };
        assert_eq!(
            f.exhausted_rounds_reason(),
            TerminalSplitReason::NoConsensus
        );
    }
}
