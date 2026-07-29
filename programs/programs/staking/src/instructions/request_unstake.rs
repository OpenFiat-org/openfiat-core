use anchor_lang::prelude::*;

use crate::{constants::*, error::ErrorCode, state::*};

/// Immediately reduces `amount` (excluded from `effective_stake` at once,
/// per OFS-4100 §4's timing decision) while the tokens themselves stay
/// locked until `unbonding_release_at`. A second `request_unstake` before
/// a prior one has been withdrawn accumulates into the same
/// `unbonding_amount` and resets `unbonding_release_at` forward — this
/// program tracks one unbonding cohort per stake account, not several
/// independently-timed ones, to keep the account layout fixed-size.
#[derive(Accounts)]
pub struct RequestUnstake<'info> {
    pub owner: Signer<'info>,

    #[account(seeds = [STAKING_CONFIG_SEED], bump = staking_config.bump)]
    pub staking_config: Account<'info, StakingConfig>,

    #[account(
        mut,
        seeds = [STAKE_ACCOUNT_SEED, owner.key().as_ref(), &[stake_account.role as u8]],
        bump = stake_account.bump,
        has_one = owner,
    )]
    pub stake_account: Account<'info, StakeAccount>,
}

pub fn handle_request_unstake(ctx: Context<RequestUnstake>, amount: u64) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let staking_config = &ctx.accounts.staking_config;
    let stake_account = &mut ctx.accounts.stake_account;

    require!(
        stake_account.amount >= amount,
        ErrorCode::InsufficientStakedAmount
    );

    // Enforcing the minimum only on the way in would leave the same hole
    // on the way out: stake the minimum, unstake one lamport, and keep an
    // account that still reads as staked while no longer qualifying.
    // Either clear the minimum or leave entirely — never the middle.
    let remaining = stake_account.amount - amount;
    require!(
        staking_config.is_legal_balance(stake_account.role, remaining),
        ErrorCode::StakeBelowRoleMinimum
    );

    stake_account.amount = remaining;
    stake_account.unbonding_amount = stake_account
        .unbonding_amount
        .checked_add(amount)
        .ok_or(ErrorCode::Overflow)?;
    stake_account.unbonding_release_at = now
        .checked_add(staking_config.unbonding_period_secs)
        .ok_or(ErrorCode::Overflow)?;
    Ok(())
}
