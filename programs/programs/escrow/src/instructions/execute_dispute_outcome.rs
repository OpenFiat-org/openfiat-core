use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, Token2022, TokenAccount};
use openfiat_programs_shared::{DisputeOutcome, VaultState};

use crate::instructions::shared_logic::{
    release_trade_escrow_funds, split_trade_escrow_evenly, unwind_funded_trade_escrow,
};
use crate::{constants::*, error::ErrorCode, state::*};

/// Permissionless, callable once the reveal window has closed — tallies
/// `dispute_case`'s own on-chain-recorded, stake-weighted votes itself
/// rather than trusting a caller-supplied outcome (Phase 4b, plan
/// decision #2).
///
/// `BuyerWins` releases funds identically to `release_escrow`.
/// `MerchantWins` and `InvalidDispute` return them to the liquidity
/// vault. `MutualSettlement` splits evenly — previously it fell in with
/// the merchant-wins arm, which handed the seller everything under a
/// verdict that by name means neither side wholly won.
///
/// A round that decides nothing pays nobody and re-opens the case; see
/// `handle_undecided_round`.
#[derive(Accounts)]
pub struct ExecuteDisputeOutcome<'info> {
    pub mint: InterfaceAccount<'info, Mint>,

    #[account(
        mut,
        seeds = [DISPUTE_CASE_SEED, &dispute_case.reservation_id.to_le_bytes()],
        bump = dispute_case.bump,
        constraint = !dispute_case.resolved @ ErrorCode::DisputeAlreadyResolved,
    )]
    pub dispute_case: Box<Account<'info, DisputeCase>>,

    #[account(
        mut,
        seeds = [TRADE_ESCROW_SEED, &trade_escrow.reservation_id.to_le_bytes()],
        bump = trade_escrow.bump,
        constraint = trade_escrow.key() == dispute_case.trade_escrow,
        constraint = trade_escrow.mint == mint.key(),
        constraint = trade_escrow.state == VaultState::Frozen @ ErrorCode::InvalidVaultState,
    )]
    pub trade_escrow: Box<Account<'info, TradeEscrowVault>>,

    #[account(
        mut,
        seeds = [TRADE_ESCROW_TOKENS_SEED, &trade_escrow.reservation_id.to_le_bytes()],
        bump = trade_escrow.token_vault_bump,
    )]
    pub trade_escrow_token_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        mut,
        seeds = [LIQUIDITY_VAULT_SEED, trade_escrow.seller.as_ref(), mint.key().as_ref()],
        bump = liquidity_vault.bump,
        constraint = liquidity_vault.mint == mint.key(),
    )]
    pub liquidity_vault: Box<Account<'info, LiquidityVault>>,

    #[account(
        mut,
        seeds = [LIQUIDITY_VAULT_TOKENS_SEED, trade_escrow.seller.as_ref(), mint.key().as_ref()],
        bump = liquidity_vault.token_vault_bump,
    )]
    pub liquidity_token_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut, constraint = buyer_token_account.owner == trade_escrow.buyer, constraint = buyer_token_account.mint == mint.key())]
    pub buyer_token_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        seeds = [FEE_CONFIG_SEED],
        bump = fee_config.bump,
        constraint = fee_config.dev_treasury == dev_treasury.key(),
        constraint = fee_config.ecosystem_treasury == ecosystem_treasury.key(),
        constraint = fee_config.infra_treasury == infra_treasury.key(),
        constraint = fee_config.emergency_reserve == emergency_reserve.key(),
    )]
    pub fee_config: Box<Account<'info, FeeConfig>>,

    #[account(mut)]
    pub dev_treasury: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub ecosystem_treasury: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub infra_treasury: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub emergency_reserve: Box<InterfaceAccount<'info, TokenAccount>>,

    pub token_program: Program<'info, Token2022>,
}

/// Sums each revealed vote's weight per outcome and returns the winner,
/// or `None` when the round reached no decision.
///
/// Zero-weight reveals are skipped outright rather than counted as votes
/// worth nothing. Counting them was load-bearing in the seat-squatting
/// attack `commit_dispute_vote` now gates against: with every weight
/// zero, all four totals tied at zero and the tie resolved to a real
/// outcome. Skipping them is the second line of defence, so the tally is
/// safe even if a zero-stake account ever reaches it again.
///
/// A genuine tie between two funded outcomes also returns `None`. It used
/// to resolve to `InvalidDispute`, which pays the seller — meaning any
/// party able to manufacture indecision was paid for it. Deciding nothing
/// must never be worth more than losing.
fn tally(dispute_case: &DisputeCase) -> Option<DisputeOutcome> {
    let mut totals = [0u128; 4];
    let mut any = false;
    for (outcome, weight) in dispute_case
        .revealed_outcomes
        .iter()
        .zip(dispute_case.weights.iter())
    {
        if *weight == 0 {
            continue;
        }
        if let Some(outcome) = outcome {
            totals[*outcome as usize] += *weight as u128;
            any = true;
        }
    }
    if !any {
        return None;
    }

    let max = *totals.iter().max().unwrap();
    let winners: Vec<usize> = totals
        .iter()
        .enumerate()
        .filter(|(_, t)| **t == max)
        .map(|(i, _)| i)
        .collect();
    if winners.len() != 1 {
        return None;
    }
    Some(match winners[0] {
        0 => DisputeOutcome::BuyerWins,
        1 => DisputeOutcome::MerchantWins,
        2 => DisputeOutcome::MutualSettlement,
        _ => DisputeOutcome::InvalidDispute,
    })
}

pub fn handle_execute_dispute_outcome(ctx: Context<ExecuteDisputeOutcome>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    require!(
        now >= ctx.accounts.dispute_case.reveal_deadline,
        ErrorCode::RevealWindowStillOpen
    );

    let Some(outcome) = tally(&ctx.accounts.dispute_case) else {
        return handle_undecided_round(ctx, now);
    };

    match outcome {
        DisputeOutcome::BuyerWins => {
            release_trade_escrow_funds(
                &mut ctx.accounts.trade_escrow,
                &ctx.accounts.trade_escrow_token_vault,
                &ctx.accounts.buyer_token_account,
                &mut ctx.accounts.liquidity_vault,
                &ctx.accounts.fee_config,
                &ctx.accounts.dev_treasury,
                &ctx.accounts.ecosystem_treasury,
                &ctx.accounts.infra_treasury,
                &ctx.accounts.emergency_reserve,
                &ctx.accounts.mint,
                &ctx.accounts.token_program,
            )?;
        }
        // Both are verdicts a stake-weighted majority actively chose:
        // the trade stands and the escrow returns to the seller. This is
        // no longer where an undecided round lands.
        DisputeOutcome::MerchantWins | DisputeOutcome::InvalidDispute => {
            unwind_funded_trade_escrow(
                &ctx.accounts.trade_escrow,
                &ctx.accounts.trade_escrow_token_vault,
                &mut ctx.accounts.liquidity_vault,
                &ctx.accounts.liquidity_token_vault,
                &ctx.accounts.mint,
                &ctx.accounts.token_program,
            )?;
            ctx.accounts.trade_escrow.state = VaultState::Cancelled;
        }
        DisputeOutcome::MutualSettlement => {
            split_trade_escrow_evenly(
                &mut ctx.accounts.trade_escrow,
                &ctx.accounts.trade_escrow_token_vault,
                &ctx.accounts.buyer_token_account,
                &mut ctx.accounts.liquidity_vault,
                &ctx.accounts.liquidity_token_vault,
                &ctx.accounts.mint,
                &ctx.accounts.token_program,
            )?;
        }
    }

    ctx.accounts.dispute_case.resolved = true;
    Ok(())
}

/// A round that decided nothing — no qualifying reveal, or a genuine tie.
///
/// Paying either party here is what made the seat-squatting attack worth
/// running, so this does not. Instead the case re-opens for another round
/// with the windows it was created with, leaving the escrow frozen and
/// the funds untouched. Honest arbitrators who missed the first round get
/// another chance; an attacker who filled the seats has to fund and lock
/// the minimum stake again, every round, to keep achieving nothing.
///
/// The retry is bounded, because "re-open forever" is the same permanent
/// freeze by another name. At the limit the escrow splits evenly, so
/// neither side profits from driving a case into the ground — both lose
/// half. See [`MAX_DISPUTE_ROUNDS`] and `split_trade_escrow_evenly`; both
/// the bound and the terminal policy are `[PROPOSED — NEEDS SIGN-OFF]`.
fn handle_undecided_round(ctx: Context<ExecuteDisputeOutcome>, now: i64) -> Result<()> {
    let next_round = ctx
        .accounts
        .dispute_case
        .round
        .checked_add(1)
        .ok_or(ErrorCode::Overflow)?;

    if next_round >= MAX_DISPUTE_ROUNDS {
        split_trade_escrow_evenly(
            &mut ctx.accounts.trade_escrow,
            &ctx.accounts.trade_escrow_token_vault,
            &ctx.accounts.buyer_token_account,
            &mut ctx.accounts.liquidity_vault,
            &ctx.accounts.liquidity_token_vault,
            &ctx.accounts.mint,
            &ctx.accounts.token_program,
        )?;
        ctx.accounts.dispute_case.resolved = true;
        return Ok(());
    }

    let dispute_case = &mut ctx.accounts.dispute_case;
    let commit_deadline = now
        .checked_add(dispute_case.commit_window_secs)
        .ok_or(ErrorCode::Overflow)?;
    let reveal_deadline = commit_deadline
        .checked_add(dispute_case.reveal_window_secs)
        .ok_or(ErrorCode::Overflow)?;

    dispute_case.round = next_round;
    dispute_case.commit_deadline = commit_deadline;
    dispute_case.reveal_deadline = reveal_deadline;
    dispute_case.arbitrators = Vec::new();
    dispute_case.commitments = Vec::new();
    dispute_case.revealed_outcomes = Vec::new();
    dispute_case.weights = Vec::new();
    Ok(())
}
