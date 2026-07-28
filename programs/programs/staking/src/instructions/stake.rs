use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    transfer_checked, Mint, Token2022, TokenAccount, TransferChecked,
};

use crate::{constants::*, error::ErrorCode, state::*};

#[derive(Accounts)]
pub struct Stake<'info> {
    pub owner: Signer<'info>,

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

    #[account(mut, constraint = from.mint == mint.key())]
    pub from: InterfaceAccount<'info, TokenAccount>,

    pub mint: InterfaceAccount<'info, Mint>,

    pub token_program: Program<'info, Token2022>,
}

pub fn handle_stake(ctx: Context<Stake>, amount: u64) -> Result<()> {
    transfer_checked(
        CpiContext::new(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: ctx.accounts.from.to_account_info(),
                mint: ctx.accounts.mint.to_account_info(),
                to: ctx.accounts.stake_vault.to_account_info(),
                authority: ctx.accounts.owner.to_account_info(),
            },
        ),
        amount,
        ctx.accounts.mint.decimals,
    )?;

    let stake_account = &mut ctx.accounts.stake_account;
    stake_account.amount = stake_account
        .amount
        .checked_add(amount)
        .ok_or(ErrorCode::Overflow)?;
    Ok(())
}
