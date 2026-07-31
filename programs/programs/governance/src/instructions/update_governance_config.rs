use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, TokenAccount};

use crate::{constants::*, error::ErrorCode, events::GovernanceConfigUpdated, state::*};

/// Corrects the singleton `GovernanceConfig` after
/// `initialize_governance_config` has run — admin-only, matching that
/// account's own "governance-updatable in a later phase, for now
/// updatable only by `admin`" note.
///
/// `forfeit_destination` arrives as a **typed token account constrained
/// to the configured mint**, not as the plain `Pubkey` param
/// `initialize_governance_config` takes. That difference is why this
/// instruction exists: the deployed config was initialized with a
/// treasury *owner* wallet, and `refund_or_forfeit_deposit` loads
/// `forfeit_destination` as a `TokenAccount` unconditionally — so not
/// just the forfeit branch but the **whole instruction**, refunds
/// included, could never execute. Nothing rejected the bad value at
/// write time, because nothing checked it. See OFS-4200 §7.
///
/// This is a little stricter than `escrow`'s `update_fee_config`, which
/// can only force its treasuries to agree with *each other* because
/// `FeeConfig` records no mint of its own. `GovernanceConfig` does record
/// one, so the mint is checked against it here and the destination is
/// then checked against the mint — closing the gap that instruction
/// documents. A destination that could not receive a forfeited deposit
/// is therefore not storable.
#[derive(Accounts)]
pub struct UpdateGovernanceConfig<'info> {
    pub admin: Signer<'info>,

    #[account(
        mut,
        seeds = [GOVERNANCE_CONFIG_SEED],
        bump = governance_config.bump,
        constraint = governance_config.admin == admin.key() @ ErrorCode::Unauthorized,
        constraint = governance_config.mint == mint.key() @ ErrorCode::MintMismatch,
    )]
    pub governance_config: Box<Account<'info, GovernanceConfig>>,

    /// The configured deposit mint. Constrained above to equal
    /// `governance_config.mint` rather than taken on trust, so this
    /// cannot be used to smuggle in a destination denominated in some
    /// other token.
    pub mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(token::mint = mint)]
    pub forfeit_destination: Box<InterfaceAccount<'info, TokenAccount>>,
}

/// The numeric half of the config. `forfeit_destination` is absent
/// deliberately — it comes from the account context above so it cannot
/// be mistyped. `mint` and `admin` are absent too: changing the mint
/// would orphan the deposit vault that already holds proposal stakes in
/// the old one, and handing over admin is a distinct action from
/// correcting parameters.
#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct UpdateGovernanceConfigParams {
    pub total_open_supply: u64,
    pub quorum_bps: u16,
    pub threshold_simple_bps: u16,
    pub threshold_treasury_bps: u16,
    pub threshold_upgrade_bps: u16,
    pub quorum_upgrade_bps: u16,
    pub deposit_amount: u64,
    pub vote_lock_secs: i64,
}

fn require_valid_bps(bps: u16) -> Result<()> {
    require!(bps as u64 <= BPS_DENOMINATOR, ErrorCode::InvalidBps);
    Ok(())
}

pub fn handle_update_governance_config(
    ctx: Context<UpdateGovernanceConfig>,
    params: UpdateGovernanceConfigParams,
) -> Result<()> {
    require_valid_bps(params.quorum_bps)?;
    require_valid_bps(params.threshold_simple_bps)?;
    require_valid_bps(params.threshold_treasury_bps)?;
    require_valid_bps(params.threshold_upgrade_bps)?;
    require_valid_bps(params.quorum_upgrade_bps)?;
    crate::shared_logic::require_valid_vote_lock(params.vote_lock_secs)?;

    let admin = ctx.accounts.admin.key();
    let forfeit_destination = ctx.accounts.forfeit_destination.key();
    let now = Clock::get()?.unix_timestamp;

    let governance_config = &mut ctx.accounts.governance_config;
    governance_config.total_open_supply = params.total_open_supply;
    governance_config.quorum_bps = params.quorum_bps;
    governance_config.threshold_simple_bps = params.threshold_simple_bps;
    governance_config.threshold_treasury_bps = params.threshold_treasury_bps;
    governance_config.threshold_upgrade_bps = params.threshold_upgrade_bps;
    governance_config.quorum_upgrade_bps = params.quorum_upgrade_bps;
    governance_config.deposit_amount = params.deposit_amount;
    governance_config.forfeit_destination = forfeit_destination;
    governance_config.vote_lock_secs = params.vote_lock_secs;

    emit!(GovernanceConfigUpdated {
        governance_config: governance_config.key(),
        admin,
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
