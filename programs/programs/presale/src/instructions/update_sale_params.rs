use anchor_lang::prelude::*;

use crate::{constants::*, error::ErrorCode, state::*};

/// Admin-only adjustment of a live sale's economic bounds (hard/soft cap,
/// per-wallet min/max, slippage tolerance) — everything except the mint,
/// vaults, swap program, sale window and stablecoin whitelist, which stay
/// fixed once initialized. Only callable while the sale is still `Active`:
/// once finalized or resolved to `SoftCapMissed`, changing these numbers
/// could make already-recorded contributions or claims inconsistent with
/// the terms buyers contributed under.
#[derive(Accounts)]
#[instruction(sale_nonce: u64)]
pub struct UpdateSaleParams<'info> {
    pub admin: Signer<'info>,

    #[account(
        mut,
        seeds = [SALE_CONFIG_SEED, &sale_nonce.to_le_bytes()],
        bump = sale_config.bump,
        has_one = admin @ ErrorCode::Unauthorized,
    )]
    pub sale_config: Account<'info, SaleConfig>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct UpdateSaleParamsArgs {
    pub hard_cap: u64,
    pub soft_cap: u64,
    pub min_contribution: u64,
    pub max_contribution: u64,
    pub max_slippage_bps: u16,
}

pub fn handle_update_sale_params(
    ctx: Context<UpdateSaleParams>,
    _sale_nonce: u64,
    params: UpdateSaleParamsArgs,
) -> Result<()> {
    let sale_config = &mut ctx.accounts.sale_config;

    require!(
        sale_config.state == SaleState::Active,
        ErrorCode::SaleAlreadyResolved
    );
    require!(
        params.hard_cap > params.soft_cap,
        ErrorCode::HardCapNotGreaterThanSoftCap
    );
    require!(
        params.min_contribution > 0 && params.min_contribution <= params.max_contribution,
        ErrorCode::InvalidContributionBounds
    );
    require!(
        params.max_slippage_bps > 0 && (params.max_slippage_bps as u64) <= BPS_DENOMINATOR,
        ErrorCode::InvalidSlippageBps
    );
    // Never lower the hard cap below what's already been raised — that
    // would retroactively put the sale over its own limit.
    require!(
        params.hard_cap >= sale_config.total_raised,
        ErrorCode::HardCapExceeded
    );

    sale_config.hard_cap = params.hard_cap;
    sale_config.soft_cap = params.soft_cap;
    sale_config.min_contribution = params.min_contribution;
    sale_config.max_contribution = params.max_contribution;
    sale_config.max_slippage_bps = params.max_slippage_bps;
    Ok(())
}
