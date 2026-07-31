//! Validation shared by `initialize_staking_config` and
//! `update_staking_config` — kept out of both so the two cannot come to
//! disagree about what a legal `StakingConfig` is.
//!
//! That disagreement is not hypothetical for this account. Every defect
//! this program has had to be upgraded for so far was a value that one
//! write path accepted and another could not use: a `slash_destination`
//! with no account behind it, a `rewards_authority` nobody held. A rule
//! enforced on the way in but not on the way back in is the same class of
//! hole with an extra step.

use anchor_lang::prelude::*;
use openfiat_programs_shared::Role;

use crate::error::ErrorCode;

/// Every role's unbonding period must be positive.
///
/// Zero is rejected rather than treated as "no unbonding" because the two
/// are indistinguishable in the stored account and only one of them is
/// ever intended. A role whose period is accidentally left at zero would
/// release its stake in the same slot it was requested, removing the
/// window in which misconduct discovered after the fact still has stake to
/// bite on — silently, and only for that one role, which is precisely the
/// kind of gap a flat single field could not have.
///
/// Negative is rejected for the sharper reason: `request_unstake` adds the
/// period to `now`, so a negative value would produce an
/// `unbonding_release_at` already in the past and `withdraw_unstaked`
/// would pay out immediately.
pub fn require_valid_unbonding_periods(periods: &[i64; Role::COUNT]) -> Result<()> {
    require!(
        periods.iter().all(|secs| *secs > 0),
        ErrorCode::InvalidUnbondingPeriod
    );
    Ok(())
}
