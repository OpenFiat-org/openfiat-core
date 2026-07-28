use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, Token2022, TokenAccount};
use openfiat_programs_shared::VaultState;

use crate::{
    constants::*,
    error::ErrorCode,
    instructions::shared_logic::{release_reservation_marking, unwind_funded_trade_escrow},
    state::*,
};

/// Permissionless once `timeout_at` has passed (OFS-2300 §8a's payment
/// window default, 30 minutes) — no signer required, matching
/// `openfiat-presale`'s permissionless-after-a-deadline pattern (there,
/// `finalize_sale`).
#[derive(Accounts)]
pub struct ExpireReservation<'info> {
    pub mint: InterfaceAccount<'info, Mint>,

    #[account(
        mut,
        seeds = [LIQUIDITY_VAULT_SEED, trade_escrow.seller.as_ref(), mint.key().as_ref()],
        bump = liquidity_vault.bump,
        constraint = liquidity_vault.mint == mint.key(),
    )]
    pub liquidity_vault: Account<'info, LiquidityVault>,

    #[account(
        mut,
        seeds = [TRADE_ESCROW_SEED, &trade_escrow.reservation_id.to_le_bytes()],
        bump = trade_escrow.bump,
        constraint = trade_escrow.mint == mint.key(),
        constraint = !trade_escrow.approved @ ErrorCode::InvalidVaultState,
        constraint = trade_escrow.state == VaultState::Reserved || trade_escrow.state == VaultState::AwaitingFiatSettlement @ ErrorCode::InvalidVaultState,
    )]
    pub trade_escrow: Account<'info, TradeEscrowVault>,

    #[account(
        mut,
        seeds = [TRADE_ESCROW_TOKENS_SEED, &trade_escrow.reservation_id.to_le_bytes()],
        bump = trade_escrow.token_vault_bump,
    )]
    pub trade_escrow_token_vault: InterfaceAccount<'info, TokenAccount>,

    #[account(
        mut,
        seeds = [LIQUIDITY_VAULT_TOKENS_SEED, trade_escrow.seller.as_ref(), mint.key().as_ref()],
        bump = liquidity_vault.token_vault_bump,
    )]
    pub liquidity_token_vault: InterfaceAccount<'info, TokenAccount>,

    pub token_program: Program<'info, Token2022>,
}

pub fn handle_expire_reservation(ctx: Context<ExpireReservation>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    require!(
        now > ctx.accounts.trade_escrow.timeout_at,
        ErrorCode::NotYetExpired
    );

    let amount = ctx.accounts.trade_escrow.amount;

    if ctx.accounts.trade_escrow.state == VaultState::AwaitingFiatSettlement {
        unwind_funded_trade_escrow(
            &ctx.accounts.trade_escrow,
            &ctx.accounts.trade_escrow_token_vault,
            &mut ctx.accounts.liquidity_vault,
            &ctx.accounts.liquidity_token_vault,
            &ctx.accounts.mint,
            &ctx.accounts.token_program,
        )?;
    } else {
        release_reservation_marking(&mut ctx.accounts.liquidity_vault, amount)?;
    }

    ctx.accounts.trade_escrow.state = VaultState::Cancelled;
    Ok(())
}
