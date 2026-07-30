use anchor_lang::prelude::*;

use crate::{constants::*, error::ErrorCode, state::*};

#[derive(Accounts)]
pub struct InitializeFeeConfig<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,

    #[account(
        init,
        payer = admin,
        space = 8 + FeeConfig::INIT_SPACE,
        seeds = [FEE_CONFIG_SEED],
        bump
    )]
    pub fee_config: Account<'info, FeeConfig>,

    pub system_program: Program<'info, System>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InitializeFeeConfigParams {
    pub ad_listing_fee: u64,
    pub dispute_filing_fee: u64,
    pub settlement_fee_bps: u16,
    pub dev_treasury: Pubkey,
    pub ecosystem_treasury: Pubkey,
    pub infra_treasury: Pubkey,
    pub emergency_reserve: Pubkey,
    pub dev_treasury_bps: u16,
    pub ecosystem_treasury_bps: u16,
    pub infra_treasury_bps: u16,
    pub emergency_reserve_bps: u16,
    pub timeout_secs: i64,
}

pub fn handle_initialize_fee_config(
    ctx: Context<InitializeFeeConfig>,
    params: InitializeFeeConfigParams,
) -> Result<()> {
    require!(
        params.settlement_fee_bps as u64 <= BPS_DENOMINATOR,
        ErrorCode::InvalidFeeBps
    );
    let split_total = params.dev_treasury_bps as u64
        + params.ecosystem_treasury_bps as u64
        + params.infra_treasury_bps as u64
        + params.emergency_reserve_bps as u64;
    require!(split_total == BPS_DENOMINATOR, ErrorCode::InvalidFeeSplit);
    require!(params.timeout_secs > 0, ErrorCode::InvalidTimeout);

    let fee_config = &mut ctx.accounts.fee_config;
    fee_config.admin = ctx.accounts.admin.key();
    fee_config.ad_listing_fee = params.ad_listing_fee;
    fee_config.dispute_filing_fee = params.dispute_filing_fee;
    fee_config.settlement_fee_bps = params.settlement_fee_bps;
    fee_config.dev_treasury = params.dev_treasury;
    fee_config.ecosystem_treasury = params.ecosystem_treasury;
    fee_config.infra_treasury = params.infra_treasury;
    fee_config.emergency_reserve = params.emergency_reserve;
    fee_config.dev_treasury_bps = params.dev_treasury_bps;
    fee_config.ecosystem_treasury_bps = params.ecosystem_treasury_bps;
    fee_config.infra_treasury_bps = params.infra_treasury_bps;
    fee_config.emergency_reserve_bps = params.emergency_reserve_bps;
    fee_config.timeout_secs = params.timeout_secs;
    fee_config.bump = ctx.bumps.fee_config;
    // Both arbitrator-eligibility gates start disabled, and are not
    // instruction parameters at all — see
    // `RECOMMENDED_MIN_ARBITRATOR_STAKE_AGE_SECS` for why zero is the only
    // value that can be true on a chain younger than the requirement it
    // would impose. Governance turns them on via `update_fee_config` once
    // the arbitrator pool has aged and is large enough to draw from.
    fee_config.min_arbitrator_stake_age_secs = 0;
    fee_config.arbitrator_sortition_bps = 0;

    // The settlement allowlist ships populated, unlike the two gates above,
    // and the asymmetry is deliberate. An empty allowlist is not an
    // inert default — it refuses every trade, so the protocol would deploy
    // switched off and stay that way until a governance write. The two
    // arbitrator gates start at zero because zero means "no requirement";
    // here the equivalent of "no requirement" is a populated list, and the
    // steward's directive is precisely what it should be populated with.
    fee_config.settlement_mints = [Pubkey::default(); MAX_SETTLEMENT_MINTS];
    fee_config.settlement_mints[..DEFAULT_SETTLEMENT_MINTS.len()]
        .copy_from_slice(&DEFAULT_SETTLEMENT_MINTS);
    fee_config.settlement_mint_count = DEFAULT_SETTLEMENT_MINTS.len() as u8;
    Ok(())
}
