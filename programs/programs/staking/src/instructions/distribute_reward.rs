use anchor_lang::prelude::*;

use crate::{constants::*, error::ErrorCode, events::RewardDistributed, state::*};

/// Plan decision #4: callable only by `rewards_authority` — a trusted
/// off-chain "reward cranker" that has already verified (via gossip-
/// observed `BlockhashAnnounced` history) whether the target node was
/// genuinely `RpcConnected` for this epoch, and computed `amount`
/// accordingly (bigger for `RpcConnected`, per the user's direct
/// instruction — see memory `staking_rewards_solana_connectivity`).
/// This instruction does no connectivity verification itself — it only
/// trusts and records the already-decided amount, matching every other
/// "off-chain decides, on-chain program executes" split in this
/// workspace (chain-bridge relay, dispute-vote relay).
#[derive(Accounts)]
pub struct DistributeReward<'info> {
    #[account(constraint = rewards_authority.key() == staking_config.rewards_authority @ ErrorCode::NotRewardsAuthority)]
    pub rewards_authority: Signer<'info>,

    #[account(seeds = [STAKING_CONFIG_SEED], bump = staking_config.bump)]
    pub staking_config: Account<'info, StakingConfig>,

    #[account(
        mut,
        seeds = [STAKE_ACCOUNT_SEED, stake_account.owner.as_ref(), &[stake_account.role as u8]],
        bump = stake_account.bump,
    )]
    pub stake_account: Account<'info, StakeAccount>,
}

pub fn handle_distribute_reward(
    ctx: Context<DistributeReward>,
    epoch: u64,
    amount: u64,
) -> Result<()> {
    let stake_account = &mut ctx.accounts.stake_account;
    stake_account.pending_rewards = stake_account
        .pending_rewards
        .checked_add(amount)
        .ok_or(ErrorCode::Overflow)?;

    emit!(RewardDistributed {
        stake_account: stake_account.key(),
        owner: stake_account.owner,
        role: stake_account.role,
        epoch,
        amount,
        pending_rewards: stake_account.pending_rewards,
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}
