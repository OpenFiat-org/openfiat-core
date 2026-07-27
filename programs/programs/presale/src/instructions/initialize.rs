use anchor_lang::prelude::*;

use crate::{constants::*, state::SaleConfig};

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(
        init,
        payer = admin,
        space = 8 + SaleConfig::INIT_SPACE,
        seeds = [SALE_CONFIG_SEED],
        bump
    )]
    pub sale_config: Account<'info, SaleConfig>,
    pub system_program: Program<'info, System>,
}

pub fn handle_initialize(ctx: Context<Initialize>) -> Result<()> {
    ctx.accounts.sale_config.admin = ctx.accounts.admin.key();
    ctx.accounts.sale_config.bump = ctx.bumps.sale_config;
    Ok(())
}
