use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    transfer_checked, Mint, Token2022, TokenAccount, TransferChecked,
};
use openfiat_programs_shared::VaultState;

use crate::{constants::*, error::ErrorCode, state::*};

#[derive(Accounts)]
pub struct FundTradeEscrow<'info> {
    pub merchant: Signer<'info>,

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
        mut,
        seeds = [LIQUIDITY_VAULT_TOKENS_SEED, merchant.key().as_ref(), mint.key().as_ref()],
        bump = liquidity_vault.token_vault_bump,
    )]
    pub liquidity_token_vault: InterfaceAccount<'info, TokenAccount>,

    #[account(
        mut,
        seeds = [TRADE_ESCROW_SEED, &trade_escrow.reservation_id.to_le_bytes()],
        bump = trade_escrow.bump,
        constraint = trade_escrow.seller == merchant.key(),
        constraint = trade_escrow.mint == mint.key(),
        constraint = trade_escrow.state == VaultState::Reserved @ ErrorCode::InvalidVaultState,
    )]
    pub trade_escrow: Account<'info, TradeEscrowVault>,

    #[account(
        mut,
        seeds = [TRADE_ESCROW_TOKENS_SEED, &trade_escrow.reservation_id.to_le_bytes()],
        bump = trade_escrow.token_vault_bump,
    )]
    pub trade_escrow_token_vault: InterfaceAccount<'info, TokenAccount>,

    pub token_program: Program<'info, Token2022>,
}

pub fn handle_fund_trade_escrow(ctx: Context<FundTradeEscrow>) -> Result<()> {
    let amount = ctx.accounts.trade_escrow.amount;
    require!(
        ctx.accounts.liquidity_vault.reserved >= amount,
        ErrorCode::InsufficientReservedLiquidity
    );

    let merchant_key = ctx.accounts.merchant.key();
    let mint_key = ctx.accounts.mint.key();
    let bump = ctx.accounts.liquidity_vault.bump;
    let signer_seeds: &[&[u8]] = &[
        LIQUIDITY_VAULT_SEED,
        merchant_key.as_ref(),
        mint_key.as_ref(),
        &[bump],
    ];

    transfer_checked(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: ctx.accounts.liquidity_token_vault.to_account_info(),
                mint: ctx.accounts.mint.to_account_info(),
                to: ctx.accounts.trade_escrow_token_vault.to_account_info(),
                authority: ctx.accounts.liquidity_vault.to_account_info(),
            },
            &[signer_seeds],
        ),
        amount,
        ctx.accounts.mint.decimals,
    )?;

    let liquidity_vault = &mut ctx.accounts.liquidity_vault;
    liquidity_vault.reserved -= amount;
    liquidity_vault.pending_settlement = liquidity_vault
        .pending_settlement
        .checked_add(amount)
        .ok_or(ErrorCode::Overflow)?;

    ctx.accounts.trade_escrow.state = VaultState::AwaitingFiatSettlement;
    Ok(())
}
