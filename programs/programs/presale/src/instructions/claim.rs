use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    transfer_checked, Mint, TokenAccount, TokenInterface, TransferChecked,
};

use crate::{constants::*, error::ErrorCode, state::*};

#[derive(Accounts)]
#[instruction(sale_nonce: u64)]
pub struct Claim<'info> {
    pub buyer: Signer<'info>,

    #[account(
        seeds = [SALE_CONFIG_SEED, &sale_nonce.to_le_bytes()],
        bump = sale_config.bump,
        constraint = sale_config.presale_vault == presale_vault.key(),
        constraint = sale_config.open_mint == open_mint.key(),
    )]
    pub sale_config: Account<'info, SaleConfig>,

    #[account(mint::token_program = token_program)]
    pub open_mint: InterfaceAccount<'info, Mint>,

    /// CHECK: verified via seeds/bump; signs the OPEN transfer below.
    #[account(seeds = [PRESALE_VAULT_SEED], bump)]
    pub presale_vault_authority: UncheckedAccount<'info>,

    #[account(mut)]
    pub presale_vault: InterfaceAccount<'info, TokenAccount>,

    #[account(
        mut,
        seeds = [CONTRIBUTION_SEED, sale_config.key().as_ref(), buyer.key().as_ref()],
        bump = contribution.bump,
        has_one = buyer @ ErrorCode::Unauthorized,
    )]
    pub contribution: Account<'info, Contribution>,

    #[account(
        mut,
        constraint = buyer_open.owner == buyer.key(),
        constraint = buyer_open.mint == sale_config.open_mint,
    )]
    pub buyer_open: InterfaceAccount<'info, TokenAccount>,

    pub token_program: Interface<'info, TokenInterface>,
}

pub fn handle_claim(ctx: Context<Claim>, _sale_nonce: u64) -> Result<()> {
    // No finalize gate: OPEN is claimable while the sale is Active or
    // Finalized. Soundness rests on the oversell invariant — total
    // entitlements are capped at hard_cap and presale_vault holds exactly
    // that much OPEN — plus the monotonic high-water mark below.
    let contribution = &ctx.accounts.contribution;
    let unclaimed = contribution
        .open_entitlement
        .checked_sub(contribution.claimed_open)
        .ok_or(ErrorCode::Overflow)?;
    require!(unclaimed > 0, ErrorCode::NothingToClaim);

    let bump = ctx.bumps.presale_vault_authority;
    let signer_seeds: &[&[u8]] = &[PRESALE_VAULT_SEED, &[bump]];
    let open_decimals = ctx.accounts.sale_config.open_decimals;

    transfer_checked(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: ctx.accounts.presale_vault.to_account_info(),
                mint: ctx.accounts.open_mint.to_account_info(),
                to: ctx.accounts.buyer_open.to_account_info(),
                authority: ctx.accounts.presale_vault_authority.to_account_info(),
            },
            &[signer_seeds],
        ),
        unclaimed,
        open_decimals,
    )?;

    ctx.accounts.contribution.claimed_open = ctx.accounts.contribution.open_entitlement;
    Ok(())
}
