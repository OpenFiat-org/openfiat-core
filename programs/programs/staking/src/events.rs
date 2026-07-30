//! On-chain event log for everything that moves value in this program
//! (OFS-4100 §9.4).
//!
//! Before these existed the workspace emitted nothing at all, so a
//! participant had no way to reconstruct what they had earned or lost —
//! `StakeAccount` carries running totals, not history, and `claim_rewards`
//! zeroes `pending_rewards` on the way out, destroying the only evidence
//! that a reward ever existed. §9.4 makes this a hard requirement rather
//! than a convenience: an unauditable reward system is indistinguishable
//! from an arbitrary one.
//!
//! Each event therefore carries enough to be read standalone — the
//! subject, the role, the amount, and the resulting balance — so an
//! indexer replaying logs never has to fetch account state to know what
//! happened. Balances are included *after* the change, so a replay can be
//! checked against the live account at any point.

use anchor_lang::prelude::*;
use openfiat_programs_shared::Role;

/// A node's reward accrued for an epoch. Emitted by `distribute_reward`.
///
/// `epoch` is the cranker's own epoch number (OFS-4100 §9.2), recorded so
/// an indexer can group a distribution run and spot a double-pay. This
/// program does not interpret it or enforce uniqueness — idempotence is
/// the cranker's responsibility, since `distribute_reward` credits
/// additively and has no notion of which epochs it has already paid.
#[event]
pub struct RewardDistributed {
    pub stake_account: Pubkey,
    pub owner: Pubkey,
    pub role: Role,
    pub epoch: u64,
    pub amount: u64,
    /// `pending_rewards` after this credit.
    pub pending_rewards: u64,
    pub timestamp: i64,
}

/// Rewards paid out to their owner. Emitted by `claim_rewards`.
#[event]
pub struct RewardsClaimed {
    pub stake_account: Pubkey,
    pub owner: Pubkey,
    pub role: Role,
    pub amount: u64,
    pub destination: Pubkey,
    pub timestamp: i64,
}

/// Stake forfeited by the slashing authority. Emitted by `slash`.
///
/// `misconduct_code` is the caller's reason code. It is recorded here and
/// nowhere else: before this event the argument was `_`-prefixed and
/// discarded entirely, so a slash left no on-chain trace of *why* it
/// happened.
///
/// `eligible_after` is false when the remaining balance sits below the
/// role's minimum — the account keeps its tokens but confers no stake
/// weight until topped back up. See `StakeAccount::effective_stake`.
#[event]
pub struct SlashApplied {
    pub stake_account: Pubkey,
    pub owner: Pubkey,
    pub role: Role,
    pub misconduct_code: u16,
    pub amount: u64,
    /// `amount` remaining after the slash.
    pub remaining_stake: u64,
    pub slashed_total: u64,
    pub eligible_after: bool,
    pub destination: Pubkey,
    pub timestamp: i64,
}

/// OPEN added to the reward pool. Emitted by `fund_rewards_vault`.
#[event]
pub struct RewardsVaultFunded {
    pub funder: Pubkey,
    pub amount: u64,
    /// The vault's balance after this deposit, so a replay can track the
    /// pool without a separate account fetch.
    pub vault_balance: u64,
    pub timestamp: i64,
}

/// The staking config's authorities or parameters were corrected.
/// Emitted by `update_staking_config`.
///
/// Worth an event because the two authority fields decide who can slash
/// and who can pay: a silent change to either is the kind of thing an
/// operator needs to be able to notice after the fact.
#[event]
pub struct StakingConfigUpdated {
    pub admin: Pubkey,
    pub slashing_authority: Pubkey,
    pub slash_destination: Pubkey,
    pub rewards_authority: Pubkey,
    pub slash_bps: u16,
    pub unbonding_period_secs: i64,
    pub timestamp: i64,
}

/// A `StakeAccount` was grown into the layout carrying
/// [`crate::state::StakeAccount::first_staked_at`]. Emitted by
/// `migrate_stake_account`.
///
/// Worth an event because the migration is the one place a stake age is
/// written without the owner having staked anything at that moment: an
/// indexer needs to be able to tell "clock started at migration" apart
/// from "clock started when tokens arrived", and after the fact the
/// account itself no longer says which happened.
#[event]
pub struct StakeAccountMigrated {
    pub stake_account: Pubkey,
    pub owner: Pubkey,
    /// Staked balance found at migration time — zero here is why
    /// `first_staked_at` is also zero.
    pub amount: u64,
    pub first_staked_at: i64,
    pub timestamp: i64,
}
