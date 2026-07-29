use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, TokenAccount};
use openfiat_programs_shared::Role;

use crate::{constants::*, error::ErrorCode, events::StakingConfigUpdated, state::*};

/// Corrects the singleton `StakingConfig` after `initialize_staking_config`
/// has run — admin-only, matching the pattern `escrow`'s
/// `update_fee_config` established.
///
/// # Why this exists
///
/// The deployed config was initialized with a `slash_destination` that
/// made `slash` permanently unexecutable, and nothing rejected it at
/// write time because `initialize_staking_config` takes it as a plain
/// `Pubkey` param: the field held an address with **no account at all**
/// on the target cluster, while `slash` requires that same key to
/// deserialize as a `TokenAccount`. Every slash therefore failed at
/// account load, leaving the disincentive half of the arbitration
/// incentive as dead code — the identical class of defect that made
/// `release_escrow` unexecutable until `update_fee_config` fixed it.
///
/// A wallet address stored there fails the same way and is the more
/// likely mistake, since a wallet at least exists and so looks valid.
///
/// # How the account types prevent it
///
/// Rather than take a corrected `Pubkey` and hope the next writer is more
/// careful, this instruction makes the defect unrepresentable:
///
/// - `slash_destination` arrives as a mint-constrained `TokenAccount`, so
///   neither a wallet nor an address without an account can be passed
///   where a token account is required — the runtime rejects both before
///   the handler runs.
/// - both authorities are rejected if zero. A null authority is the same
///   dead configuration in a different disguise: it stores cleanly and
///   makes the instruction it gates permanently uncallable.
///
/// Requiring the authorities to *sign* this instruction would be stronger
/// still — it would prove at write time that somebody holds the key,
/// which is precisely what went wrong. It is deliberately not done: a
/// PDA cannot sign a client transaction, so that rule would make it
/// impossible to ever hand either authority to a program, and
/// governance-controlled distribution is the intended destination for
/// `rewards_authority`. A non-zero check is what can be enforced without
/// foreclosing that.
///
/// The numeric parameters stay plain — a wrong `slash_bps` is a policy
/// error, not a structurally impossible state, and it is validated below.
#[derive(Accounts)]
pub struct UpdateStakingConfig<'info> {
    pub admin: Signer<'info>,

    #[account(
        mut,
        seeds = [STAKING_CONFIG_SEED],
        bump = staking_config.bump,
        constraint = staking_config.admin == admin.key() @ ErrorCode::Unauthorized,
    )]
    pub staking_config: Box<Account<'info, StakingConfig>>,

    /// Pinned to the config's own mint: the stake vault holds this mint,
    /// so a slash destination denominated in anything else could never
    /// receive a transfer from it.
    #[account(constraint = mint.key() == staking_config.mint @ ErrorCode::WrongMint)]
    pub mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(token::mint = mint)]
    pub slash_destination: Box<InterfaceAccount<'info, TokenAccount>>,
}

/// The numeric half, plus the two authority addresses. The slash
/// destination is absent deliberately — it comes from the account context
/// so it cannot be mistyped into a wallet.
#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct UpdateStakingConfigParams {
    pub min_stake_by_role: [u64; Role::COUNT],
    pub unbonding_period_secs: i64,
    pub slash_bps: u16,
    /// Both are addresses that sign elsewhere rather than accounts read
    /// here, so they arrive as plain keys — validated non-zero below.
    pub slashing_authority: Pubkey,
    pub rewards_authority: Pubkey,
}

pub fn handle_update_staking_config(
    ctx: Context<UpdateStakingConfig>,
    params: UpdateStakingConfigParams,
) -> Result<()> {
    require!(
        params.slash_bps as u64 <= BPS_DENOMINATOR,
        ErrorCode::InvalidSlashBps
    );
    require!(
        params.unbonding_period_secs > 0,
        ErrorCode::InvalidUnbondingPeriod
    );
    require!(
        params.slashing_authority != Pubkey::default(),
        ErrorCode::ZeroAuthority
    );
    require!(
        params.rewards_authority != Pubkey::default(),
        ErrorCode::ZeroAuthority
    );

    let config = &mut ctx.accounts.staking_config;
    config.min_stake_by_role = params.min_stake_by_role;
    config.unbonding_period_secs = params.unbonding_period_secs;
    config.slash_bps = params.slash_bps;
    config.slashing_authority = params.slashing_authority;
    config.slash_destination = ctx.accounts.slash_destination.key();
    config.rewards_authority = params.rewards_authority;

    emit!(StakingConfigUpdated {
        admin: ctx.accounts.admin.key(),
        slashing_authority: config.slashing_authority,
        slash_destination: config.slash_destination,
        rewards_authority: config.rewards_authority,
        slash_bps: config.slash_bps,
        unbonding_period_secs: config.unbonding_period_secs,
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}
