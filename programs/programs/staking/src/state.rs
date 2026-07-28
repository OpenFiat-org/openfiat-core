use anchor_lang::prelude::*;
use openfiat_programs_shared::Role;

/// Singleton staking configuration (OFS-4200 §5), governance-updatable in
/// a later phase (once `openfiat-governance`'s `update_config_parameter`
/// exists) — for now, updatable only by `admin`.
#[account]
#[derive(InitSpace)]
pub struct StakingConfig {
    pub admin: Pubkey,
    pub mint: Pubkey,
    pub min_stake: u64,
    pub min_stake_arbitrator: u64,
    pub unbonding_period_secs: i64,
    pub slash_bps: u16,
    pub slashing_authority: Pubkey,
    /// Where a `slash`'s forfeited tokens go — not named in OFS-4200 §5,
    /// this workspace's own extension (mirrors `escrow::FeeConfig`'s
    /// treasury-destination pattern rather than inventing a new one).
    pub slash_destination: Pubkey,
    /// Plan decision #4: the RpcConnected/GossipOnly reward asymmetry's
    /// trusted off-chain "reward cranker" — verifies connectivity mode
    /// via gossip-observed `BlockhashAnnounced` history, then calls
    /// `distribute_reward`. Not itself named in OFS-4200 §5.
    pub rewards_authority: Pubkey,
    pub bump: u8,
    pub stake_vault_bump: u8,
    pub rewards_vault_bump: u8,
}

/// One wallet's stake under one role (OFS-4200 §5) — a wallet may hold
/// independent stakes under different roles, each its own PDA.
#[account]
#[derive(InitSpace)]
pub struct StakeAccount {
    pub owner: Pubkey,
    pub role: Role,
    /// Staked and NOT currently unbonding — see [`StakeAccount::effective_stake`].
    pub amount: u64,
    pub unbonding_amount: u64,
    pub unbonding_release_at: i64,
    pub slashed_total: u64,
    /// Plan decision #4 extension — accrued via `distribute_reward`,
    /// paid out via `claim_rewards`. Tracked separately from `amount` so
    /// a reward doesn't silently change a stake's role-eligibility math
    /// until the owner actually claims it.
    pub pending_rewards: u64,
    pub bump: u8,
}

impl StakeAccount {
    /// OFS-4200 §5's `get_effective_stake` — implemented as a plain
    /// associated function rather than a dispatched CPI instruction.
    /// `openfiat-governance`'s `cast_vote` and `openfiat-escrow`'s
    /// dispute-vote tally (Phase 4b) depend on this crate as a plain
    /// Cargo path dependency and read a `StakeAccount` account directly
    /// (identical to how any Anchor account is deserialized cross-
    /// program) rather than issuing a real CPI call — a same-result,
    /// lower-overhead pattern for a pure read, and the reason no
    /// `get_effective_stake` instruction is dispatched anywhere in this
    /// program's `#[program]` module.
    pub fn effective_stake(&self) -> u64 {
        self.amount
    }
}
