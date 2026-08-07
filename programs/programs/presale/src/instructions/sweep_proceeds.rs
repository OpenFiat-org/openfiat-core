use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    transfer_checked, Mint, TokenAccount, TokenInterface, TransferChecked,
};

use crate::{constants::*, error::ErrorCode, state::*};

/// Emitted on every successful sweep so contributors can watch proceeds
/// move to the published treasury on-chain.
#[event]
pub struct ProceedsSwept {
    pub sale_config: Pubkey,
    pub treasury: Pubkey,
    pub amount: u64,
    pub vault_remaining: u64,
}

/// Admin-gated draw-down of collected USDC to the sale's fixed treasury
/// while the sale is still Active. The destination is constrained to
/// `sale_config.treasury`: the admin controls *when* to sweep, never
/// *where* the funds go. Sound only because there is no refund path — see
/// the claim-anytime / soft_cap=0 design (OFS-4100 §3).
#[derive(Accounts)]
#[instruction(sale_nonce: u64)]
pub struct SweepProceeds<'info> {
    pub admin: Signer<'info>,

    #[account(
        mut,
        seeds = [SALE_CONFIG_SEED, &sale_nonce.to_le_bytes()],
        bump = sale_config.bump,
        has_one = admin @ ErrorCode::Unauthorized,
        constraint = sale_config.usdc_vault == usdc_vault.key(),
        constraint = sale_config.treasury == treasury.key(),
        constraint = sale_config.usdc_mint == usdc_mint.key(),
    )]
    pub sale_config: Account<'info, SaleConfig>,

    #[account(mut)]
    pub usdc_vault: InterfaceAccount<'info, TokenAccount>,

    #[account(mut)]
    pub treasury: InterfaceAccount<'info, TokenAccount>,

    #[account(mint::token_program = token_program)]
    pub usdc_mint: InterfaceAccount<'info, Mint>,

    pub token_program: Interface<'info, TokenInterface>,
}

pub fn handle_sweep_proceeds(
    ctx: Context<SweepProceeds>,
    sale_nonce: u64,
    amount: u64,
) -> Result<()> {
    require!(
        ctx.accounts.sale_config.state == SaleState::Active,
        ErrorCode::SaleAlreadyResolved
    );

    let vault_amount = ctx.accounts.usdc_vault.amount;
    require!(
        amount > 0 && amount <= vault_amount,
        ErrorCode::InvalidSweepAmount
    );

    let bump = ctx.accounts.sale_config.bump;
    let usdc_decimals = ctx.accounts.sale_config.usdc_decimals;
    let nonce_bytes = sale_nonce.to_le_bytes();
    let signer_seeds: &[&[u8]] = &[SALE_CONFIG_SEED, &nonce_bytes, &[bump]];

    transfer_checked(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: ctx.accounts.usdc_vault.to_account_info(),
                mint: ctx.accounts.usdc_mint.to_account_info(),
                to: ctx.accounts.treasury.to_account_info(),
                authority: ctx.accounts.sale_config.to_account_info(),
            },
            &[signer_seeds],
        ),
        amount,
        usdc_decimals,
    )?;

    emit!(ProceedsSwept {
        sale_config: ctx.accounts.sale_config.key(),
        treasury: ctx.accounts.treasury.key(),
        amount,
        vault_remaining: vault_amount - amount,
    });
    Ok(())
}
