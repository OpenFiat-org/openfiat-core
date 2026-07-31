use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, TokenAccount, TokenInterface};

use crate::instructions::initialize_emergency_authority::write_emergency_authority;
use crate::{
    constants::*,
    error::ErrorCode,
    events::{EmergencyAuthorityInitialized, GovernanceConfigInitialized},
    state::*,
};

#[derive(Accounts)]
pub struct InitializeGovernanceConfig<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,

    #[account(mint::token_program = token_program)]
    pub mint: InterfaceAccount<'info, Mint>,

    #[account(
        init,
        payer = admin,
        space = 8 + GovernanceConfig::INIT_SPACE,
        seeds = [GOVERNANCE_CONFIG_SEED],
        bump
    )]
    pub governance_config: Account<'info, GovernanceConfig>,

    #[account(
        init,
        payer = admin,
        seeds = [DEPOSIT_VAULT_SEED],
        bump,
        token::mint = mint,
        token::authority = governance_config,
        token::token_program = token_program,
    )]
    pub deposit_vault: InterfaceAccount<'info, TokenAccount>,

    /// AllenHark's first-year exception, created here so its clock starts
    /// at governance genesis — OFS-4100 §5.1 measures the window from
    /// "initialization", and this is that moment.
    ///
    /// Created atomically with the config rather than left to a follow-up
    /// transaction because `update_governance_config` requires it: a
    /// deployment that had a config but no `EmergencyAuthority` would
    /// have an unconfigurable governance program. It takes no parameters,
    /// so this adds nothing to `InitializeGovernanceConfigParams` and no
    /// existing caller has to change — Anchor resolves the address from
    /// its constant seed.
    #[account(
        init,
        payer = admin,
        space = 8 + EmergencyAuthority::INIT_SPACE,
        seeds = [EMERGENCY_AUTHORITY_SEED],
        bump
    )]
    pub emergency_authority: Account<'info, EmergencyAuthority>,

    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InitializeGovernanceConfigParams {
    pub total_open_supply: u64,
    pub quorum_bps: u16,
    pub threshold_simple_bps: u16,
    pub threshold_treasury_bps: u16,
    pub threshold_upgrade_bps: u16,
    pub quorum_upgrade_bps: u16,
    pub deposit_amount: u64,
    pub forfeit_destination: Pubkey,
    pub vote_lock_secs: i64,
}

fn require_valid_bps(bps: u16) -> Result<()> {
    require!(bps as u64 <= BPS_DENOMINATOR, ErrorCode::InvalidBps);
    Ok(())
}

pub fn handle_initialize_governance_config(
    ctx: Context<InitializeGovernanceConfig>,
    params: InitializeGovernanceConfigParams,
) -> Result<()> {
    require_valid_bps(params.quorum_bps)?;
    require_valid_bps(params.threshold_simple_bps)?;
    require_valid_bps(params.threshold_treasury_bps)?;
    require_valid_bps(params.threshold_upgrade_bps)?;
    require_valid_bps(params.quorum_upgrade_bps)?;
    crate::shared_logic::require_valid_vote_lock(params.vote_lock_secs)?;

    let deposit_vault = ctx.accounts.deposit_vault.key();
    let now = Clock::get()?.unix_timestamp;

    // The sunset is set here, from a compiled-in duration, and nothing in
    // this program ever writes it again.
    let emergency_authority = &mut ctx.accounts.emergency_authority;
    write_emergency_authority(emergency_authority, now, ctx.bumps.emergency_authority)?;
    emit!(EmergencyAuthorityInitialized {
        emergency_authority: emergency_authority.key(),
        primary_holder: emergency_authority.primary_holder,
        secondary_holder: emergency_authority.secondary_holder,
        initialized_at: emergency_authority.initialized_at,
        expires_at: emergency_authority.expires_at,
    });

    let governance_config = &mut ctx.accounts.governance_config;
    governance_config.admin = ctx.accounts.admin.key();
    governance_config.mint = ctx.accounts.mint.key();
    governance_config.total_open_supply = params.total_open_supply;
    governance_config.quorum_bps = params.quorum_bps;
    governance_config.threshold_simple_bps = params.threshold_simple_bps;
    governance_config.threshold_treasury_bps = params.threshold_treasury_bps;
    governance_config.threshold_upgrade_bps = params.threshold_upgrade_bps;
    governance_config.quorum_upgrade_bps = params.quorum_upgrade_bps;
    governance_config.deposit_amount = params.deposit_amount;
    governance_config.forfeit_destination = params.forfeit_destination;
    governance_config.vote_lock_secs = params.vote_lock_secs;
    governance_config.bump = ctx.bumps.governance_config;
    governance_config.deposit_vault_bump = ctx.bumps.deposit_vault;

    emit!(GovernanceConfigInitialized {
        governance_config: governance_config.key(),
        admin: governance_config.admin,
        mint: governance_config.mint,
        deposit_vault,
        total_open_supply: governance_config.total_open_supply,
        quorum_bps: governance_config.quorum_bps,
        threshold_simple_bps: governance_config.threshold_simple_bps,
        threshold_treasury_bps: governance_config.threshold_treasury_bps,
        threshold_upgrade_bps: governance_config.threshold_upgrade_bps,
        quorum_upgrade_bps: governance_config.quorum_upgrade_bps,
        deposit_amount: governance_config.deposit_amount,
        forfeit_destination: governance_config.forfeit_destination,
        vote_lock_secs: governance_config.vote_lock_secs,
        timestamp: now,
    });
    Ok(())
}
