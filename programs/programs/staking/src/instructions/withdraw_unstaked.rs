use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    transfer_checked, Mint, Token2022, TokenAccount, TransferChecked,
};

use crate::{constants::*, error::ErrorCode, state::*};

#[derive(Accounts)]
pub struct WithdrawUnstaked<'info> {
    pub owner: Signer<'info>,

    pub mint: InterfaceAccount<'info, Mint>,

    #[account(
        seeds = [STAKING_CONFIG_SEED],
        bump = staking_config.bump,
        constraint = staking_config.mint == mint.key(),
    )]
    pub staking_config: Account<'info, StakingConfig>,

    #[account(
        mut,
        seeds = [STAKE_ACCOUNT_SEED, owner.key().as_ref(), &[stake_account.role as u8]],
        bump = stake_account.bump,
        has_one = owner,
    )]
    pub stake_account: Account<'info, StakeAccount>,

    #[account(mut, seeds = [STAKE_VAULT_SEED], bump = staking_config.stake_vault_bump)]
    pub stake_vault: InterfaceAccount<'info, TokenAccount>,

    #[account(mut, constraint = to.mint == mint.key())]
    pub to: InterfaceAccount<'info, TokenAccount>,

    pub token_program: Program<'info, Token2022>,
}

pub fn handle_withdraw_unstaked(ctx: Context<WithdrawUnstaked>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let stake_account = &ctx.accounts.stake_account;
    require!(
        stake_account.unbonding_amount > 0,
        ErrorCode::NoUnbondingBalance
    );
    require!(
        now >= stake_account.unbonding_release_at,
        ErrorCode::StillUnbonding
    );

    let amount = stake_account.unbonding_amount;
    let bump = ctx.accounts.staking_config.bump;
    let signer_seeds: &[&[u8]] = &[STAKING_CONFIG_SEED, &[bump]];

    transfer_checked(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: ctx.accounts.stake_vault.to_account_info(),
                mint: ctx.accounts.mint.to_account_info(),
                to: ctx.accounts.to.to_account_info(),
                authority: ctx.accounts.staking_config.to_account_info(),
            },
            &[signer_seeds],
        ),
        amount,
        ctx.accounts.mint.decimals,
    )?;

    ctx.accounts.stake_account.unbonding_amount = 0;
    Ok(())
}
