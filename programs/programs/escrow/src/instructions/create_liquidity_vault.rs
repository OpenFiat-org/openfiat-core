use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, Token2022, TokenAccount};

use crate::{constants::*, state::*};

#[derive(Accounts)]
pub struct CreateLiquidityVault<'info> {
    #[account(mut)]
    pub merchant: Signer<'info>,

    pub mint: InterfaceAccount<'info, Mint>,

    #[account(
        init,
        payer = merchant,
        space = 8 + LiquidityVault::INIT_SPACE,
        seeds = [LIQUIDITY_VAULT_SEED, merchant.key().as_ref(), mint.key().as_ref()],
        bump
    )]
    pub liquidity_vault: Account<'info, LiquidityVault>,

    #[account(
        init,
        payer = merchant,
        seeds = [LIQUIDITY_VAULT_TOKENS_SEED, merchant.key().as_ref(), mint.key().as_ref()],
        bump,
        token::mint = mint,
        token::authority = liquidity_vault,
        token::token_program = token_program,
    )]
    pub token_vault: InterfaceAccount<'info, TokenAccount>,

    pub token_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

pub fn handle_create_liquidity_vault(ctx: Context<CreateLiquidityVault>) -> Result<()> {
    let liquidity_vault = &mut ctx.accounts.liquidity_vault;
    liquidity_vault.merchant = ctx.accounts.merchant.key();
    liquidity_vault.mint = ctx.accounts.mint.key();
    liquidity_vault.total = 0;
    liquidity_vault.reserved = 0;
    liquidity_vault.available = 0;
    liquidity_vault.settled = 0;
    liquidity_vault.pending_settlement = 0;
    liquidity_vault.bump = ctx.bumps.liquidity_vault;
    liquidity_vault.token_vault_bump = ctx.bumps.token_vault;
    Ok(())
}
