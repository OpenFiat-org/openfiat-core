use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    transfer_checked, Mint, Token2022, TokenAccount, TransferChecked,
};

use crate::{constants::*, error::ErrorCode, state::*};

#[derive(Accounts)]
#[instruction(sale_nonce: u64)]
pub struct FinalizeSale<'info> {
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

    pub usdc_mint: InterfaceAccount<'info, Mint>,

    pub token_program: Program<'info, Token2022>,
}

pub fn handle_finalize_sale(ctx: Context<FinalizeSale>, sale_nonce: u64) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let sale_config = &ctx.accounts.sale_config;

    require!(
        sale_config.state == SaleState::Active,
        ErrorCode::SaleAlreadyResolved
    );
    require!(
        now > sale_config.end_time || sale_config.total_raised >= sale_config.hard_cap,
        ErrorCode::SaleNotEnded
    );

    let soft_cap_met = sale_config.total_raised >= sale_config.soft_cap;
    let bump = sale_config.bump;
    let usdc_decimals = sale_config.usdc_decimals;
    let nonce_bytes = sale_nonce.to_le_bytes();
    let signer_seeds: &[&[u8]] = &[SALE_CONFIG_SEED, &nonce_bytes, &[bump]];

    if soft_cap_met {
        let amount = ctx.accounts.usdc_vault.amount;
        if amount > 0 {
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
        }
    }

    let sale_config = &mut ctx.accounts.sale_config;
    sale_config.state = if soft_cap_met {
        SaleState::Finalized
    } else {
        SaleState::SoftCapMissed
    };

    Ok(())
}
