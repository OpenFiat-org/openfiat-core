use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, Token2022, TokenAccount};
use openfiat_programs_shared::{DisputeOutcome, VaultState};

use crate::instructions::shared_logic::{release_trade_escrow_funds, unwind_funded_trade_escrow};
use crate::{constants::*, error::ErrorCode, state::*};

/// Permissionless, callable once the reveal window has closed — tallies
/// `dispute_case`'s own on-chain-recorded, stake-weighted votes itself
/// rather than trusting a caller-supplied outcome (Phase 4b, plan
/// decision #2). `BuyerWins` releases funds identically to
/// `release_escrow`; `MerchantWins`/`InvalidDispute`/`MutualSettlement`
/// all return funds to the liquidity vault — a real amount-split for
/// `MutualSettlement` would need each party's agreed ratio recorded
/// somewhere on-chain, which nothing here does yet (OFS-2400 §17's
/// "Partial Settlement" is explicitly future work), so this
/// implementation treats it the same as a non-release outcome for now.
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

/// Sums each revealed vote's weight per outcome and returns the winner.
/// A tie (including the all-zero/no-reveals case, caught separately by
/// the caller) resolves to `InvalidDispute` — the same safe-default
/// pattern `openfiat-governance`'s own vote tally already uses for a
/// quorum miss or a genuine weight tie.
fn tally(dispute_case: &DisputeCase) -> Option<DisputeOutcome> {
    let mut totals = [0u128; 4];
    let mut any = false;
    for (outcome, weight) in dispute_case
        .revealed_outcomes
        .iter()
        .zip(dispute_case.weights.iter())
    {
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
        return Some(DisputeOutcome::InvalidDispute);
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

    let outcome = tally(&ctx.accounts.dispute_case).ok_or(ErrorCode::NoVotesRevealed)?;

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
        DisputeOutcome::MerchantWins
        | DisputeOutcome::MutualSettlement
        | DisputeOutcome::InvalidDispute => {
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
    }

    ctx.accounts.dispute_case.resolved = true;
    Ok(())
}
