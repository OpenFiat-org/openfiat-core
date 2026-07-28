use anchor_lang::prelude::*;
use openfiat_programs_shared::Role;

use crate::{constants::*, state::*};

#[derive(Accounts)]
#[instruction(role: Role)]
pub struct InitializeStakeAccount<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        init,
        payer = owner,
        space = 8 + StakeAccount::INIT_SPACE,
        seeds = [STAKE_ACCOUNT_SEED, owner.key().as_ref(), &[role as u8]],
        bump
    )]
    pub stake_account: Account<'info, StakeAccount>,

    pub system_program: Program<'info, System>,
}

pub fn handle_initialize_stake_account(
    ctx: Context<InitializeStakeAccount>,
    role: Role,
) -> Result<()> {
    let stake_account = &mut ctx.accounts.stake_account;
    stake_account.owner = ctx.accounts.owner.key();
    stake_account.role = role;
    stake_account.amount = 0;
    stake_account.unbonding_amount = 0;
    stake_account.unbonding_release_at = 0;
    stake_account.slashed_total = 0;
    stake_account.pending_rewards = 0;
    stake_account.bump = ctx.bumps.stake_account;
    Ok(())
}
