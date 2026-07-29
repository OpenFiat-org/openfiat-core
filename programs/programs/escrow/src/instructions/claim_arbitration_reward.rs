use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    transfer_checked, Mint, Token2022, TokenAccount, TransferChecked,
};

use crate::{constants::*, error::ErrorCode, events::ArbitrationRewardClaimed, state::*};

/// Pays one arbitrator their pro-rata share of a forfeited arbitration
/// deposit (OFS-4100 §9.3).
///
/// This is the incentive the commit-reveal design has always assumed and
/// never had. Before it, an arbitrator paid transaction fees to stake,
/// commit and reveal, and received nothing whichever way they voted —
/// while voting *against* the eventual consensus also cost nothing. The
/// mechanism was symmetric in exactly the wrong direction.
///
/// # Pull, not push
///
/// Each arbitrator claims for themselves rather than being paid out by
/// `execute_dispute_outcome`. Pushing would mean that instruction taking
/// up to [`MAX_ARBITRATORS`] token accounts it cannot know in advance, on
/// top of the ten-plus it already carries, and one arbitrator with a
/// closed token account would fail the whole resolution — leaving the
/// escrow frozen because a payout failed. Resolution and payment are
/// separate concerns and separate transactions.
///
/// # Share
///
/// Pro-rata by the weight recorded at reveal, over the total weight behind
/// the winning outcome. The final claimant takes the truncation remainder,
/// so integer division never strands dust in the pool: `reward_remaining`
/// is paid out in full to whoever claims last.
///
/// Only arbitrators who revealed *the winning outcome* may claim. Voting
/// with the minority earns nothing here, which alongside slashing is the
/// asymmetry that makes honest voting the paying strategy.
#[derive(Accounts)]
pub struct ClaimArbitrationReward<'info> {
    pub arbitrator: Signer<'info>,

    #[account(
        mut,
        seeds = [DISPUTE_CASE_SEED, &dispute_case.reservation_id.to_le_bytes()],
        bump = dispute_case.bump,
        constraint = dispute_case.resolved @ ErrorCode::DisputeNotDecided,
    )]
    pub dispute_case: Box<Account<'info, DisputeCase>>,

    #[account(
        seeds = [FEE_CONFIG_SEED],
        bump = fee_config.bump,
    )]
    pub fee_config: Box<Account<'info, FeeConfig>>,

    #[account(constraint = mint.key() == dispute_case.deposit_mint)]
    pub mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(
        mut,
        seeds = [ARBITRATION_POOL_SEED],
        bump,
        constraint = arbitration_pool.mint == mint.key(),
    )]
    pub arbitration_pool: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut, constraint = to.mint == mint.key())]
    pub to: Box<InterfaceAccount<'info, TokenAccount>>,

    pub token_program: Program<'info, Token2022>,
}

pub fn handle_claim_arbitration_reward(ctx: Context<ClaimArbitrationReward>) -> Result<()> {
    let case = &ctx.accounts.dispute_case;
    let outcome = case.outcome.ok_or(ErrorCode::DisputeNotDecided)?;
    require!(case.reward_remaining > 0, ErrorCode::NoFeeConfigured);

    let arbitrator = ctx.accounts.arbitrator.key();
    let index = case
        .arbitrators
        .iter()
        .position(|a| *a == arbitrator)
        .ok_or(ErrorCode::NotAWinningArbitrator)?;

    require!(
        case.revealed_outcomes.get(index).copied().flatten() == Some(outcome),
        ErrorCode::NotAWinningArbitrator
    );
    require!(
        !case.reward_claimed.get(index).copied().unwrap_or(true),
        ErrorCode::RewardAlreadyClaimed
    );

    let weight = case.weights.get(index).copied().unwrap_or(0);
    require!(weight > 0, ErrorCode::NotAWinningArbitrator);

    // Whoever claims last takes the remainder, so basis-point truncation
    // cannot strand dust in the pool.
    let outstanding = case
        .reward_claimed
        .iter()
        .enumerate()
        .filter(|(i, claimed)| {
            !**claimed
                && case.revealed_outcomes.get(*i).copied().flatten() == Some(outcome)
                && case.weights.get(*i).copied().unwrap_or(0) > 0
        })
        .count();

    let amount = if outstanding <= 1 {
        case.reward_remaining
    } else {
        let share = (case.reward_pool as u128)
            .checked_mul(weight as u128)
            .ok_or(ErrorCode::Overflow)?
            .checked_div(case.winning_weight.max(1) as u128)
            .ok_or(ErrorCode::Overflow)? as u64;
        share.min(case.reward_remaining)
    };

    let fee_bump = ctx.accounts.fee_config.bump;
    let signer_seeds: &[&[u8]] = &[FEE_CONFIG_SEED, &[fee_bump]];
    transfer_checked(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: ctx.accounts.arbitration_pool.to_account_info(),
                mint: ctx.accounts.mint.to_account_info(),
                to: ctx.accounts.to.to_account_info(),
                authority: ctx.accounts.fee_config.to_account_info(),
            },
            &[signer_seeds],
        ),
        amount,
        ctx.accounts.mint.decimals,
    )?;

    let case = &mut ctx.accounts.dispute_case;
    case.reward_claimed[index] = true;
    case.reward_remaining = case
        .reward_remaining
        .checked_sub(amount)
        .ok_or(ErrorCode::Overflow)?;

    emit!(ArbitrationRewardClaimed {
        reservation_id: case.reservation_id,
        arbitrator,
        weight,
        winning_weight: case.winning_weight,
        amount,
        destination: ctx.accounts.to.key(),
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}
