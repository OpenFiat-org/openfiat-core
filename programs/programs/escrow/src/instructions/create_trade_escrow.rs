use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, Token2022, TokenAccount};
use openfiat_programs_shared::VaultState;

use crate::{constants::*, error::ErrorCode, state::*};

#[derive(Accounts)]
#[instruction(reservation_id: u64)]
pub struct CreateTradeEscrow<'info> {
    /// The seller — same wallet as `LiquidityVault.merchant`.
    #[account(mut)]
    pub merchant: Signer<'info>,

    /// The buyer's wallet — not a signer here (the off-chain Reservation
    /// Protocol, OFS-2200, already established their intent to trade
    /// against this merchant's published ad terms); recorded so
    /// `release_escrow` knows who to pay.
    /// CHECK: recorded verbatim, never read/written.
    pub buyer: UncheckedAccount<'info>,

    pub mint: InterfaceAccount<'info, Mint>,

    #[account(
        mut,
        seeds = [LIQUIDITY_VAULT_SEED, merchant.key().as_ref(), mint.key().as_ref()],
        bump = liquidity_vault.bump,
        has_one = merchant,
        constraint = liquidity_vault.mint == mint.key(),
    )]
    pub liquidity_vault: Account<'info, LiquidityVault>,

    #[account(
        init,
        payer = merchant,
        space = 8 + TradeEscrowVault::INIT_SPACE,
        seeds = [TRADE_ESCROW_SEED, &reservation_id.to_le_bytes()],
        bump
    )]
    pub trade_escrow: Account<'info, TradeEscrowVault>,

    #[account(
        init,
        payer = merchant,
        seeds = [TRADE_ESCROW_TOKENS_SEED, &reservation_id.to_le_bytes()],
        bump,
        token::mint = mint,
        token::authority = trade_escrow,
        token::token_program = token_program,
    )]
    pub token_vault: InterfaceAccount<'info, TokenAccount>,

    pub token_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

pub fn handle_create_trade_escrow(
    ctx: Context<CreateTradeEscrow>,
    reservation_id: u64,
    amount: u64,
    dispute_authority: Pubkey,
    timeout_secs: i64,
) -> Result<()> {
    require!(
        ctx.accounts.liquidity_vault.reserved >= amount,
        ErrorCode::InsufficientReservedLiquidity
    );
    require!(timeout_secs > 0, ErrorCode::InvalidTimeout);

    let now = Clock::get()?.unix_timestamp;
    let trade_escrow = &mut ctx.accounts.trade_escrow;
    trade_escrow.reservation_id = reservation_id;
    trade_escrow.buyer = ctx.accounts.buyer.key();
    trade_escrow.seller = ctx.accounts.merchant.key();
    trade_escrow.mint = ctx.accounts.mint.key();
    trade_escrow.amount = amount;
    trade_escrow.state = VaultState::Reserved;
    trade_escrow.approved = false;
    trade_escrow.dispute_authority = dispute_authority;
    trade_escrow.created_at = now;
    trade_escrow.timeout_at = now.checked_add(timeout_secs).ok_or(ErrorCode::Overflow)?;
    trade_escrow.bump = ctx.bumps.trade_escrow;
    trade_escrow.token_vault_bump = ctx.bumps.token_vault;
    Ok(())
}
