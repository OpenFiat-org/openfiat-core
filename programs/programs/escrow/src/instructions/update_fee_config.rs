use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, TokenAccount};

use crate::{constants::*, error::ErrorCode, state::*};

/// Corrects the singleton `FeeConfig` after `initialize_fee_config` has
/// run — admin-only, matching `FeeConfig`'s own "governance-updatable in
/// a later phase, for now updatable only by `admin`" note.
///
/// The four treasuries arrive as **typed token accounts constrained to a
/// mint**, not as plain `Pubkey` params like `initialize_fee_config`
/// takes them. That difference is the point of this instruction existing
/// at all: the deployed config was initialized with the treasury *owner*
/// wallets rather than their token accounts, and since `release_escrow`
/// requires each treasury to deserialize as a `TokenAccount`, the whole
/// release path — every settlement and every `BuyerWins` dispute — could
/// not execute. Nothing rejected the bad values at write time, because
/// nothing checked them.
///
/// Taking them as accounts means the runtime does the checking: a wallet
/// address cannot be passed where a `TokenAccount` is required, and
/// `token::mint = mint` forces all four to share one mint, so a config
/// that would fail at release time cannot be stored in the first place.
#[derive(Accounts)]
pub struct UpdateFeeConfig<'info> {
    pub admin: Signer<'info>,

    #[account(
        mut,
        seeds = [FEE_CONFIG_SEED],
        bump = fee_config.bump,
        constraint = fee_config.admin == admin.key() @ ErrorCode::Unauthorized,
    )]
    pub fee_config: Box<Account<'info, FeeConfig>>,

    /// The mint every treasury must hold. Not stored on `FeeConfig`, so
    /// this enforces the four are mutually consistent rather than
    /// checking them against a recorded mint — see this file's own note
    /// on the remaining gap.
    pub mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(token::mint = mint)]
    pub dev_treasury: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(token::mint = mint)]
    pub ecosystem_treasury: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(token::mint = mint)]
    pub infra_treasury: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(token::mint = mint)]
    pub emergency_reserve: Box<InterfaceAccount<'info, TokenAccount>>,
}

/// The numeric half of the config. The treasury addresses are absent
/// deliberately — they come from the account context above so they
/// cannot be mistyped.
#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct UpdateFeeConfigParams {
    pub ad_listing_fee: u64,
    pub dispute_filing_fee: u64,
    pub settlement_fee_bps: u16,
    pub dev_treasury_bps: u16,
    pub ecosystem_treasury_bps: u16,
    pub infra_treasury_bps: u16,
    pub emergency_reserve_bps: u16,
    pub timeout_secs: i64,
}

pub fn handle_update_fee_config(
    ctx: Context<UpdateFeeConfig>,
    params: UpdateFeeConfigParams,
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
    fee_config.ad_listing_fee = params.ad_listing_fee;
    fee_config.dispute_filing_fee = params.dispute_filing_fee;
    fee_config.settlement_fee_bps = params.settlement_fee_bps;
    fee_config.dev_treasury = ctx.accounts.dev_treasury.key();
    fee_config.ecosystem_treasury = ctx.accounts.ecosystem_treasury.key();
    fee_config.infra_treasury = ctx.accounts.infra_treasury.key();
    fee_config.emergency_reserve = ctx.accounts.emergency_reserve.key();
    fee_config.dev_treasury_bps = params.dev_treasury_bps;
    fee_config.ecosystem_treasury_bps = params.ecosystem_treasury_bps;
    fee_config.infra_treasury_bps = params.infra_treasury_bps;
    fee_config.emergency_reserve_bps = params.emergency_reserve_bps;
    fee_config.timeout_secs = params.timeout_secs;
    // `admin` is intentionally not updatable here — handing over control
    // is a distinct action from correcting fee parameters, and folding it
    // into this instruction would let one fat-fingered call lock the
    // config permanently.
    Ok(())
}
