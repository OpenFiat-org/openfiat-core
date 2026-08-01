//! The off-chain half of the stake-recovery relay: what a merchant still
//! owes on arbitration deposits their liquidity vault could not cover, and
//! what their stake can still be made to pay (OFS-4100 §9.3, OFS-4200 §1).
//!
//! # Why any of this is off-chain
//!
//! OFS-4200 §1 forbids `openfiat-escrow` calling `openfiat-staking`
//! directly, so that a bug in one program's CPI-calling code cannot
//! corrupt the other's state. §9.3 inherits that: the shortfall is
//! recorded by escrow, and a separate transaction against staking collects
//! it. Something has to notice that the first has happened and the second
//! has not. That noticing is all this module does.
//!
//! # What it deliberately does not do
//!
//! It does not sign or submit anything, for the same reason
//! `openfiat-slashing` does not: a node in this workspace never builds a
//! Solana transaction, `crates/chain` relays only already-signed bytes,
//! and the node holds no key with authority over funds.
//!
//! The difference from slashing is that here it does not matter who does.
//! `slash` is authority-gated, so its relay is a recommendation to one
//! keyholder; `recover_stake_shortfall` is permissionless and derives
//! every number from accounts it reads, so this schedule is a
//! recommendation to *anybody* — including the merchant, who is often the
//! party most motivated to clear their own debt. Nothing here is trusted
//! by the chain. If this module computed the wrong amount, the on-chain
//! instruction would still move the right one; the worst a wrong answer
//! does is waste a transaction.
//!
//! That is the property worth having, and it is why the arithmetic below
//! mirrors the on-chain arithmetic exactly rather than approximating it:
//! anyone can recompute a plan from public accounts and compare it against
//! what actually happened.

/// A merchant's arbitration-deposit debt, as recorded on
/// `escrow::StakeRecoveryClaim`.
///
/// Mirrors the on-chain account's fields rather than depending on the
/// Anchor type — the caller decodes, this module decides, exactly as
/// `openfiat-slashing` treats a resolved dispute case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RecoveryClaim {
    pub merchant: [u8; 32],
    /// The OPEN mint the debt is denominated in.
    pub mint: [u8; 32],
    /// Monotone sum of every case's shortfall. Never reduced by payment.
    pub owed_total: u64,
    /// Monotone sum of what escrow has already credited back to the
    /// merchant's vault.
    pub credited_total: u64,
    pub case_count: u32,
}

/// A merchant's Merchant-role stake position, as recorded on
/// `staking::StakeAccount`, paired with `staking::StakeRecoveryReceipt`.
///
/// `recovered_total` is zero for a merchant whose receipt account does not
/// exist yet — the same reading the on-chain instruction takes of an empty
/// account, and the common case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct MerchantStake {
    /// `StakeAccount.amount` — staked and not unbonding.
    pub active: u64,
    /// `StakeAccount.unbonding_amount` — requested but not yet withdrawn,
    /// and still sitting in the stake vault, which is what makes it
    /// reachable.
    pub unbonding: u64,
    /// `StakeRecoveryReceipt.recovered_total`.
    pub recovered_total: u64,
}

/// One `recover_stake_shortfall` worth submitting, and what it will and
/// will not settle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RecoveryPlan {
    pub merchant: [u8; 32],
    pub mint: [u8; 32],
    /// Owed less already recovered, before this plan runs.
    pub outstanding: u64,
    /// What the instruction will actually take: the outstanding amount, or
    /// everything the stake holds, whichever is smaller.
    pub recoverable: u64,
    /// Of [`Self::recoverable`], the part coming out of the unbonding
    /// cohort. Drained first on chain, so it is drained first here.
    pub from_unbonding: u64,
    /// The part coming out of the active balance, after unbonding is
    /// exhausted.
    pub from_active: u64,
    /// What will still be owed afterwards. **Non-zero is the case that
    /// matters**: the merchant's stake does not cover their debt, the
    /// arbitrators on some case will be short a reward, and no further
    /// recovery can change that until the merchant stakes more.
    pub unrecoverable: u64,
}

impl RecoveryPlan {
    /// Whether this plan leaves the debt fully settled.
    pub fn settles_in_full(&self) -> bool {
        self.unrecoverable == 0
    }
}

/// What a merchant still owes: the two monotone counters differenced,
/// exactly as both programs do it.
///
/// Saturating for the same reason the on-chain helper is: the counters are
/// written by two different programs, and if a receipt ever ran ahead of a
/// claim the right reading is "nothing outstanding" rather than a
/// arithmetic failure in the middle of a relay.
pub fn outstanding(claim: &RecoveryClaim, stake: &MerchantStake) -> u64 {
    claim.owed_total.saturating_sub(stake.recovered_total)
}

/// What `escrow::absorb_stake_recovery` would credit to the merchant's
/// vault right now — tokens staking has already moved that escrow has not
/// yet accounted for.
///
/// Worth surfacing separately from [`plan`] because the two are independent
/// transactions with independent reasons to be stuck. A merchant whose
/// stake was recovered but whose vault was never credited holds liquidity
/// they cannot spend, and no amount of further recovery fixes it.
pub fn absorbable(claim: &RecoveryClaim, stake: &MerchantStake) -> u64 {
    stake.recovered_total.saturating_sub(claim.credited_total)
}

/// The recovery worth submitting for one merchant, or `None` when there is
/// nothing to do.
///
/// `None` covers both of the on-chain instruction's refusals — nothing
/// outstanding, and nothing left to take — because from a submitter's
/// point of view they are the same instruction: one that would fail. The
/// difference between them is not lost, because a debt that cannot be
/// collected is still visible on the claim; it is simply not actionable,
/// and a relay that kept resubmitting it would burn fees forever.
pub fn plan(claim: &RecoveryClaim, stake: &MerchantStake) -> Option<RecoveryPlan> {
    let outstanding = outstanding(claim, stake);
    if outstanding == 0 {
        return None;
    }

    // Unbonding first, matching `recover_stake_shortfall`. Getting this
    // ordering wrong here would not lose money — the program decides — but
    // it would make the plan unverifiable against what actually happened,
    // and being reproducible is the only reason to compute it off-chain at
    // all.
    let from_unbonding = outstanding.min(stake.unbonding);
    let from_active = outstanding.saturating_sub(from_unbonding).min(stake.active);
    let recoverable = from_unbonding.saturating_add(from_active);
    if recoverable == 0 {
        return None;
    }

    Some(RecoveryPlan {
        merchant: claim.merchant,
        mint: claim.mint,
        outstanding,
        recoverable,
        from_unbonding,
        from_active,
        unrecoverable: outstanding - recoverable,
    })
}

/// Every recovery worth submitting, over a set of merchants.
///
/// Ordered by how much each would collect, descending, so a submitter with
/// a limited fee budget spends it where the most debt is settled. Ties
/// break on the merchant key so the ordering is total and two observers
/// with the same input produce the same list.
pub fn schedule<'a, I>(merchants: I) -> Vec<RecoveryPlan>
where
    I: IntoIterator<Item = (&'a RecoveryClaim, &'a MerchantStake)>,
{
    let mut plans: Vec<RecoveryPlan> = merchants
        .into_iter()
        .filter_map(|(claim, stake)| plan(claim, stake))
        .collect();
    plans.sort_by(|a, b| {
        b.recoverable
            .cmp(&a.recoverable)
            .then_with(|| a.merchant.cmp(&b.merchant))
    });
    plans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim(owed: u64, credited: u64) -> RecoveryClaim {
        RecoveryClaim {
            merchant: [7u8; 32],
            mint: [9u8; 32],
            owed_total: owed,
            credited_total: credited,
            case_count: 1,
        }
    }

    #[test]
    fn a_merchant_who_owes_nothing_has_no_plan() {
        let stake = MerchantStake {
            active: 1_000,
            ..Default::default()
        };
        assert_eq!(plan(&claim(0, 0), &stake), None);
    }

    #[test]
    fn a_settled_debt_has_no_plan_even_though_it_is_still_recorded() {
        let stake = MerchantStake {
            active: 1_000,
            unbonding: 0,
            recovered_total: 40,
        };
        // `owed_total` never comes down, so "settled" is only visible as
        // the difference. A plan keyed on `owed_total > 0` would resubmit
        // this forever.
        assert_eq!(plan(&claim(40, 40), &stake), None);
    }

    #[test]
    fn unbonding_is_taken_before_the_active_balance() {
        let stake = MerchantStake {
            active: 500,
            unbonding: 30,
            recovered_total: 0,
        };
        let plan = plan(&claim(100, 0), &stake).expect("a plan");
        assert_eq!(plan.from_unbonding, 30);
        assert_eq!(plan.from_active, 70);
        assert_eq!(plan.recoverable, 100);
        assert!(plan.settles_in_full());
    }

    #[test]
    fn unbonding_alone_can_cover_the_debt() {
        let stake = MerchantStake {
            active: 500,
            unbonding: 400,
            recovered_total: 0,
        };
        let plan = plan(&claim(100, 0), &stake).expect("a plan");
        assert_eq!(plan.from_unbonding, 100);
        assert_eq!(plan.from_active, 0);
    }

    #[test]
    fn a_stake_that_cannot_cover_the_debt_reports_the_remainder() {
        let stake = MerchantStake {
            active: 10,
            unbonding: 5,
            recovered_total: 0,
        };
        let plan = plan(&claim(100, 0), &stake).expect("a plan");
        assert_eq!(plan.recoverable, 15);
        assert_eq!(plan.unrecoverable, 85);
        assert!(!plan.settles_in_full());
    }

    #[test]
    fn an_empty_stake_yields_no_plan_but_the_debt_stands() {
        let stake = MerchantStake::default();
        let claim = claim(100, 0);
        assert_eq!(plan(&claim, &stake), None);
        // Not actionable is not the same as not owed — the withdrawal gate
        // stays shut on this merchant, and the claim still says 100.
        assert_eq!(outstanding(&claim, &stake), 100);
    }

    #[test]
    fn a_partial_recovery_leaves_the_rest_recoverable_later() {
        let claim = claim(100, 0);
        let first = MerchantStake {
            active: 40,
            unbonding: 0,
            recovered_total: 0,
        };
        let plan_one = plan(&claim, &first).expect("a plan");
        assert_eq!(plan_one.recoverable, 40);

        // The merchant tops their stake back up; the receipt now records
        // the first pass.
        let second = MerchantStake {
            active: 200,
            unbonding: 0,
            recovered_total: 40,
        };
        let plan_two = plan(&claim, &second).expect("a plan");
        assert_eq!(plan_two.outstanding, 60);
        assert_eq!(plan_two.recoverable, 60);
        assert!(plan_two.settles_in_full());
    }

    #[test]
    fn absorbable_is_what_staking_moved_and_escrow_has_not_booked() {
        let stake = MerchantStake {
            recovered_total: 40,
            ..Default::default()
        };
        assert_eq!(absorbable(&claim(100, 0), &stake), 40);
        assert_eq!(absorbable(&claim(100, 40), &stake), 0);
        // Escrow can never lead staking, but if it somehow did, this is
        // "nothing to book" rather than a panic.
        assert_eq!(absorbable(&claim(100, 90), &stake), 0);
    }

    #[test]
    fn a_receipt_ahead_of_its_claim_reads_as_settled_rather_than_failing() {
        let stake = MerchantStake {
            active: 100,
            recovered_total: 500,
            ..Default::default()
        };
        assert_eq!(outstanding(&claim(100, 0), &stake), 0);
        assert_eq!(plan(&claim(100, 0), &stake), None);
    }

    #[test]
    fn the_schedule_collects_what_is_actionable_largest_first() {
        let small = RecoveryClaim {
            merchant: [1u8; 32],
            owed_total: 20,
            ..claim(20, 0)
        };
        let large = RecoveryClaim {
            merchant: [2u8; 32],
            owed_total: 900,
            ..claim(900, 0)
        };
        let paid = RecoveryClaim {
            merchant: [3u8; 32],
            ..claim(50, 0)
        };
        let funded = MerchantStake {
            active: 10_000,
            ..Default::default()
        };
        let settled = MerchantStake {
            active: 10_000,
            recovered_total: 50,
            ..Default::default()
        };

        let plans = schedule(vec![
            (&small, &funded),
            (&large, &funded),
            (&paid, &settled),
        ]);
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0].merchant, [2u8; 32]);
        assert_eq!(plans[0].recoverable, 900);
        assert_eq!(plans[1].merchant, [1u8; 32]);
    }
}
