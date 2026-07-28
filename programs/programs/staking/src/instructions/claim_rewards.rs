use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    transfer_checked, Mint, Token2022, TokenAccount, TransferChecked,
};

use crate::{constants::*, error::ErrorCode, state::*};

#[derive(Accounts)]
pub struct ClaimRewards<'info> {
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

    #[account(mut, seeds = [REWARDS_VAULT_SEED], bump = staking_config.rewards_vault_bump)]
    pub rewards_vault: InterfaceAccount<'info, TokenAccount>,

    #[account(mut, constraint = to.mint == mint.key())]
    pub to: InterfaceAccount<'info, TokenAccount>,

    pub token_program: Program<'info, Token2022>,
}

pub fn handle_claim_rewards(ctx: Context<ClaimRewards>) -> Result<()> {
    let amount = ctx.accounts.stake_account.pending_rewards;
    require!(amount > 0, ErrorCode::NoPendingRewards);

    let bump = ctx.accounts.staking_config.bump;
    let signer_seeds: &[&[u8]] = &[STAKING_CONFIG_SEED, &[bump]];

    transfer_checked(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: ctx.accounts.rewards_vault.to_account_info(),
                mint: ctx.accounts.mint.to_account_info(),
                to: ctx.accounts.to.to_account_info(),
                authority: ctx.accounts.staking_config.to_account_info(),
            },
            &[signer_seeds],
        ),
        amount,
        ctx.accounts.mint.decimals,
    )?;

    ctx.accounts.stake_account.pending_rewards = 0;
    Ok(())
}
