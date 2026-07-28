use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, Token2022, TokenAccount};

use crate::{constants::*, error::ErrorCode, state::*};

#[derive(Accounts)]
pub struct InitializeStakingConfig<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,

    pub mint: InterfaceAccount<'info, Mint>,

    #[account(
        init,
        payer = admin,
        space = 8 + StakingConfig::INIT_SPACE,
        seeds = [STAKING_CONFIG_SEED],
        bump
    )]
    pub staking_config: Account<'info, StakingConfig>,

    #[account(
        init,
        payer = admin,
        seeds = [STAKE_VAULT_SEED],
        bump,
        token::mint = mint,
        token::authority = staking_config,
        token::token_program = token_program,
    )]
    pub stake_vault: InterfaceAccount<'info, TokenAccount>,

    #[account(
        init,
        payer = admin,
        seeds = [REWARDS_VAULT_SEED],
        bump,
        token::mint = mint,
        token::authority = staking_config,
        token::token_program = token_program,
    )]
    pub rewards_vault: InterfaceAccount<'info, TokenAccount>,

    pub token_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InitializeStakingConfigParams {
    pub min_stake: u64,
    pub min_stake_arbitrator: u64,
    pub unbonding_period_secs: i64,
    pub slash_bps: u16,
    pub slashing_authority: Pubkey,
    pub slash_destination: Pubkey,
    pub rewards_authority: Pubkey,
}

pub fn handle_initialize_staking_config(
    ctx: Context<InitializeStakingConfig>,
    params: InitializeStakingConfigParams,
) -> Result<()> {
    require!(
        params.slash_bps as u64 <= BPS_DENOMINATOR,
        ErrorCode::InvalidSlashBps
    );
    require!(
        params.unbonding_period_secs > 0,
        ErrorCode::InvalidUnbondingPeriod
    );

    let staking_config = &mut ctx.accounts.staking_config;
    staking_config.admin = ctx.accounts.admin.key();
    staking_config.mint = ctx.accounts.mint.key();
    staking_config.min_stake = params.min_stake;
    staking_config.min_stake_arbitrator = params.min_stake_arbitrator;
    staking_config.unbonding_period_secs = params.unbonding_period_secs;
    staking_config.slash_bps = params.slash_bps;
    staking_config.slashing_authority = params.slashing_authority;
    staking_config.slash_destination = params.slash_destination;
    staking_config.rewards_authority = params.rewards_authority;
    staking_config.bump = ctx.bumps.staking_config;
    staking_config.stake_vault_bump = ctx.bumps.stake_vault;
    staking_config.rewards_vault_bump = ctx.bumps.rewards_vault;
    Ok(())
}
