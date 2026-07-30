pub mod constants;
pub mod error;
pub mod events;
pub mod instructions;
pub mod state;

use anchor_lang::prelude::*;
use openfiat_programs_shared::Role;

pub use constants::*;
pub use events::*;
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

    /// One-shot layout migration for the existing devnet deployment — see
    /// `migrate_staking_config`'s own doc comment.
    pub fn migrate_staking_config(
        ctx: Context<MigrateStakingConfig>,
        min_stake_by_role: [u64; Role::COUNT],
    ) -> Result<()> {
        crate::instructions::migrate_staking_config::handle_migrate_staking_config(
            ctx,
            min_stake_by_role,
        )
    }

    /// One-shot per-account layout migration adding
    /// `StakeAccount.first_staked_at` — see `migrate_stake_account`'s own
    /// doc comment for why it is permissionless and why the age clock
    /// starts at migration rather than at deployment.
    pub fn migrate_stake_account(ctx: Context<MigrateStakeAccount>) -> Result<()> {
        crate::instructions::migrate_stake_account::handle_migrate_stake_account(ctx)
    }

    /// Corrects the singleton config's authorities and parameters.
    /// Admin-only; see `instructions::update_staking_config` for why the
    /// slash destination arrives as an account rather than a key.
    pub fn update_staking_config(
        ctx: Context<UpdateStakingConfig>,
        params: UpdateStakingConfigParams,
    ) -> Result<()> {
        instructions::update_staking_config::handle_update_staking_config(ctx, params)
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

    /// `epoch` is the reward cranker's own epoch number — recorded in the
    /// emitted event so a distribution run is groupable and a double-pay
    /// is visible. This program does not enforce uniqueness on it; see
    /// `distribute_reward`'s own doc for why idempotence stays off-chain.
    pub fn distribute_reward(
        ctx: Context<DistributeReward>,
        epoch: u64,
        amount: u64,
    ) -> Result<()> {
        crate::instructions::distribute_reward::handle_distribute_reward(ctx, epoch, amount)
    }

    /// Adds OPEN to the pool `claim_rewards` pays out of. Permissionless —
    /// see `fund_rewards_vault`'s own doc.
    pub fn fund_rewards_vault(ctx: Context<FundRewardsVault>, amount: u64) -> Result<()> {
        crate::instructions::fund_rewards_vault::handle_fund_rewards_vault(ctx, amount)
    }

    pub fn claim_rewards(ctx: Context<ClaimRewards>) -> Result<()> {
        crate::instructions::claim_rewards::handle_claim_rewards(ctx)
    }
}
