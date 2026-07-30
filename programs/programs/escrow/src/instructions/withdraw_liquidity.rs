use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    transfer_checked, Mint, TokenAccount, TokenInterface, TransferChecked,
};

use crate::{constants::*, error::ErrorCode, state::*};

#[derive(Accounts)]
pub struct WithdrawLiquidity<'info> {
    pub merchant: Signer<'info>,

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
    pub token_vault: InterfaceAccount<'info, TokenAccount>,

    #[account(mut, constraint = to.mint == mint.key())]
    pub to: InterfaceAccount<'info, TokenAccount>,

    #[account(mint::token_program = token_program)]
    pub mint: InterfaceAccount<'info, Mint>,

    pub token_program: Interface<'info, TokenInterface>,
}

pub fn handle_withdraw_liquidity(ctx: Context<WithdrawLiquidity>, amount: u64) -> Result<()> {
    let liquidity_vault = &mut ctx.accounts.liquidity_vault;
    require!(
        liquidity_vault.available >= amount,
        ErrorCode::InsufficientAvailableLiquidity
    );

    let merchant_key = ctx.accounts.merchant.key();
    let mint_key = ctx.accounts.mint.key();
    let bump = liquidity_vault.bump;
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
                from: ctx.accounts.token_vault.to_account_info(),
                mint: ctx.accounts.mint.to_account_info(),
                to: ctx.accounts.to.to_account_info(),
                authority: ctx.accounts.liquidity_vault.to_account_info(),
            },
            &[signer_seeds],
        ),
        amount,
        ctx.accounts.mint.decimals,
    )?;

    let liquidity_vault = &mut ctx.accounts.liquidity_vault;
    liquidity_vault.available -= amount;
    liquidity_vault.total = liquidity_vault
        .total
        .checked_sub(amount)
        .ok_or(ErrorCode::Overflow)?;
    Ok(())
}
