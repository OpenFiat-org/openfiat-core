pub mod constants;
pub mod error;
pub mod instructions;
pub mod state;

use anchor_lang::prelude::*;
use openfiat_programs_shared::Role;

pub use constants::*;
pub use instructions::*;
pub use state::*;

declare_id!("HYEXk8XQukBkZbiYB33JyVefQDxqyCpPudad3wBCyYmx");

/// `openfiat-staking` — per-role OPEN staking, unbonding, and slashing
/// (OFS-4200 §5). Phase 5a. `get_effective_stake` is deliberately not a
/// dispatched instruction here — see `StakeAccount::effective_stake`'s
/// own doc comment for why a direct cross-program account read is the
/// idiomatic (and this program's actual) implementation.
#[program]
pub mod staking {
    use super::*;

    pub fn initialize_staking_config(
        ctx: Context<InitializeStakingConfig>,
        params: InitializeStakingConfigParams,
    ) -> Result<()> {
        crate::instructions::initialize_staking_config::handle_initialize_staking_config(
            ctx, params,
        )
    }

    pub fn initialize_stake_account(
        ctx: Context<InitializeStakeAccount>,
        role: Role,
    ) -> Result<()> {
        crate::instructions::initialize_stake_account::handle_initialize_stake_account(ctx, role)
    }

    pub fn stake(ctx: Context<Stake>, amount: u64) -> Result<()> {
        crate::instructions::stake::handle_stake(ctx, amount)
    }

    pub fn request_unstake(ctx: Context<RequestUnstake>, amount: u64) -> Result<()> {
        crate::instructions::request_unstake::handle_request_unstake(ctx, amount)
    }

    pub fn withdraw_unstaked(ctx: Context<WithdrawUnstaked>) -> Result<()> {
        crate::instructions::withdraw_unstaked::handle_withdraw_unstaked(ctx)
    }

    pub fn slash(ctx: Context<Slash>, misconduct_code: u16) -> Result<()> {
        crate::instructions::slash::handle_slash(ctx, misconduct_code)
    }

    pub fn distribute_reward(ctx: Context<DistributeReward>, amount: u64) -> Result<()> {
        crate::instructions::distribute_reward::handle_distribute_reward(ctx, amount)
    }

    pub fn claim_rewards(ctx: Context<ClaimRewards>) -> Result<()> {
        crate::instructions::claim_rewards::handle_claim_rewards(ctx)
    }
}
