use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    transfer_checked, Mint, Token2022, TokenAccount, TransferChecked,
};

use crate::{constants::*, error::ErrorCode, state::*};

#[derive(Accounts)]
pub struct DepositLiquidity<'info> {
    pub merchant: Signer<'info>,

    /// CHECK: OFS-7100 §12 deposit gate, enforced by *proof of
    /// non-existence*. Unchecked and uninitialized on purpose — the
    /// wallet is banned iff this address is occupied, so in the passing
    /// case there is nothing to deserialize. The soundness lives in the
    /// constraint, not the type: `seeds`/`seeds::program` force this to
    /// be the one canonical ban address for `merchant` under
    /// `openfiat-governance`, so a banned caller cannot substitute an
    /// unrelated empty account and appear unbanned. Removing either line
    /// silently disables the ban for this instruction.
    #[account(
        seeds = [openfiat_programs_shared::BAN_SEED, merchant.key().as_ref()],
        bump,
        seeds::program = openfiat_programs_shared::GOVERNANCE_PROGRAM_ID,
    )]
    pub ban_record: UncheckedAccount<'info>,

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

    #[account(mut, constraint = from.mint == mint.key())]
    pub from: InterfaceAccount<'info, TokenAccount>,

    pub mint: InterfaceAccount<'info, Mint>,

    pub token_program: Program<'info, Token2022>,
}

pub fn handle_deposit_liquidity(ctx: Context<DepositLiquidity>, amount: u64) -> Result<()> {
    require!(
        !openfiat_programs_shared::wallet_is_banned(&ctx.accounts.ban_record),
        ErrorCode::WalletBanned
    );

    transfer_checked(
        CpiContext::new(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: ctx.accounts.from.to_account_info(),
                mint: ctx.accounts.mint.to_account_info(),
                to: ctx.accounts.token_vault.to_account_info(),
                authority: ctx.accounts.merchant.to_account_info(),
            },
        ),
        amount,
        ctx.accounts.mint.decimals,
    )?;

    let liquidity_vault = &mut ctx.accounts.liquidity_vault;
    liquidity_vault.total = liquidity_vault
        .total
        .checked_add(amount)
        .ok_or(ErrorCode::Overflow)?;
    liquidity_vault.available = liquidity_vault
        .available
        .checked_add(amount)
        .ok_or(ErrorCode::Overflow)?;
    Ok(())
}
