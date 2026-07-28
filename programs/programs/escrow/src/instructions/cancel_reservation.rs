use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, Token2022, TokenAccount};
use openfiat_programs_shared::VaultState;

use crate::{
    constants::*,
    error::ErrorCode,
    instructions::shared_logic::{release_reservation_marking, unwind_funded_trade_escrow},
    state::*,
};

/// OFS-2300 §19a: allowed pre-"I Paid" by either party, not allowed
/// unilaterally afterward. This program has no visibility into the
/// off-chain "I Paid" event itself (that's `crates/settlement`'s state),
/// so the on-chain approximation is: allowed by either the buyer or the
/// seller as long as `approve_settlement` hasn't run yet — approval only
/// happens after the merchant has fully reviewed a submitted payment,
/// so `approved == true` is always a point at which "I Paid" has
/// necessarily already occurred.
#[derive(Accounts)]
pub struct CancelReservation<'info> {
    #[account(constraint = signer.key() == trade_escrow.buyer || signer.key() == trade_escrow.seller @ ErrorCode::NotAPartyToThisTrade)]
    pub signer: Signer<'info>,

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

    /// Only meaningfully used when the escrow was already funded — pass
    /// the same PDA regardless for a uniform client call.
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

pub fn handle_cancel_reservation(ctx: Context<CancelReservation>) -> Result<()> {
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
