use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    transfer_checked, Mint, Token2022, TokenAccount, TransferChecked,
};

use crate::{constants::*, error::ErrorCode, state::*};

/// Refunds are paid in USDC (the post-swap asset for non-USDC contributions),
/// not in whatever the contributor originally sent — OFS-4100 §3's refund
/// semantics. The presale UI must state this plainly before a non-USDC
/// contribution is confirmed.
#[derive(Accounts)]
#[instruction(sale_nonce: u64)]
pub struct Refund<'info> {
    pub buyer: Signer<'info>,

    #[account(
        seeds = [SALE_CONFIG_SEED, &sale_nonce.to_le_bytes()],
        bump = sale_config.bump,
        constraint = sale_config.usdc_vault == usdc_vault.key(),
        constraint = sale_config.usdc_mint == usdc_mint.key(),
    )]
    pub sale_config: Account<'info, SaleConfig>,

    #[account(mut)]
    pub usdc_vault: InterfaceAccount<'info, TokenAccount>,

    pub usdc_mint: InterfaceAccount<'info, Mint>,

    #[account(
        mut,
        seeds = [CONTRIBUTION_SEED, sale_config.key().as_ref(), buyer.key().as_ref()],
        bump = contribution.bump,
        has_one = buyer @ ErrorCode::Unauthorized,
    )]
    pub contribution: Account<'info, Contribution>,

    #[account(
        mut,
        constraint = buyer_usdc.owner == buyer.key(),
        constraint = buyer_usdc.mint == sale_config.usdc_mint,
    )]
    pub buyer_usdc: InterfaceAccount<'info, TokenAccount>,

    pub token_program: Program<'info, Token2022>,
}

pub fn handle_refund(ctx: Context<Refund>, sale_nonce: u64) -> Result<()> {
    require!(
        ctx.accounts.sale_config.state == SaleState::SoftCapMissed,
        ErrorCode::SaleNotRefundable
    );
    require!(
        !ctx.accounts.contribution.refunded,
        ErrorCode::AlreadyRefunded
    );

    let bump = ctx.accounts.sale_config.bump;
    let usdc_decimals = ctx.accounts.sale_config.usdc_decimals;
    let nonce_bytes = sale_nonce.to_le_bytes();
    let signer_seeds: &[&[u8]] = &[SALE_CONFIG_SEED, &nonce_bytes, &[bump]];
    let amount = ctx.accounts.contribution.amount_usdc;

    transfer_checked(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: ctx.accounts.usdc_vault.to_account_info(),
                mint: ctx.accounts.usdc_mint.to_account_info(),
                to: ctx.accounts.buyer_usdc.to_account_info(),
                authority: ctx.accounts.sale_config.to_account_info(),
            },
            &[signer_seeds],
        ),
        amount,
        usdc_decimals,
    )?;

    ctx.accounts.contribution.refunded = true;
    Ok(())
}
