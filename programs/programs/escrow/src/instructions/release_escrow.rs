use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, TokenAccount, TokenInterface};
use openfiat_programs_shared::VaultState;

use crate::events::EscrowReleased;
use crate::instructions::shared_logic::release_trade_escrow_funds;
use crate::{constants::*, error::ErrorCode, state::*};

/// The only instruction that moves settlement funds (OFS-2300 §16).
/// Permissionless once `approve_settlement` has run — matching "the
/// merchant never manually transfers stablecoins; only the OpenFiat
/// Program releases escrow" (OFS-2300 §15), there is no further
/// merchant action required after approval, so anyone (the buyer, a
/// relaying node, an automated cranker) may trigger the release.
#[derive(Accounts)]
pub struct ReleaseEscrow<'info> {
    #[account(mint::token_program = token_program)]
    pub mint: InterfaceAccount<'info, Mint>,

    #[account(
        mut,
        seeds = [LIQUIDITY_VAULT_SEED, trade_escrow.seller.as_ref(), mint.key().as_ref()],
        bump = liquidity_vault.bump,
        constraint = liquidity_vault.mint == mint.key(),
    )]
    pub liquidity_vault: Box<Account<'info, LiquidityVault>>,

    #[account(
        mut,
        seeds = [TRADE_ESCROW_SEED, &trade_escrow.reservation_id.to_le_bytes()],
        bump = trade_escrow.bump,
        constraint = trade_escrow.mint == mint.key(),
        constraint = trade_escrow.state == VaultState::AwaitingFiatSettlement @ ErrorCode::InvalidVaultState,
        constraint = trade_escrow.approved @ ErrorCode::InvalidVaultState,
    )]
    pub trade_escrow: Box<Account<'info, TradeEscrowVault>>,

    #[account(
        mut,
        seeds = [TRADE_ESCROW_TOKENS_SEED, &trade_escrow.reservation_id.to_le_bytes()],
        bump = trade_escrow.token_vault_bump,
    )]
    pub trade_escrow_token_vault: Box<InterfaceAccount<'info, TokenAccount>>,

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

    pub token_program: Interface<'info, TokenInterface>,
}

pub fn handle_release_escrow(ctx: Context<ReleaseEscrow>) -> Result<()> {
    let amount = ctx.accounts.trade_escrow.amount;
    let (buyer_amount, fee_shares) = release_trade_escrow_funds(
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

    emit!(EscrowReleased {
        reservation_id: ctx.accounts.trade_escrow.reservation_id,
        buyer: ctx.accounts.trade_escrow.buyer,
        seller: ctx.accounts.trade_escrow.seller,
        mint: ctx.accounts.mint.key(),
        amount,
        buyer_amount,
        fee: amount
            .checked_sub(buyer_amount)
            .ok_or(ErrorCode::Overflow)?,
        dev_treasury_amount: fee_shares[0],
        ecosystem_treasury_amount: fee_shares[1],
        infra_treasury_amount: fee_shares[2],
        emergency_reserve_amount: fee_shares[3],
        via_dispute: false,
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}
