//! Shared enums (OFS-4200 §2) reused by `escrow`, `staking`, and
//! `governance` so the three programs don't each define their own
//! incompatible copy of the same protocol concept.

use anchor_lang::prelude::*;

/// A staked/bonded protocol role (OFS-4200 §2). `staking::StakeAccount`
/// is keyed by `(owner, role)` — one wallet may hold independent stakes
/// under different roles.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace, Debug)]
pub enum Role {
    Merchant,
    Arbitrator,
    NodeOperator,
    NotificationProvider,
    OracleProvider,
    RiskIntelligenceProvider,
    SnapshotProvider,
}

impl Role {
    /// Number of variants. Used as the length of per-role parameter arrays
    /// (`staking::StakingConfig::min_stake_by_role`), so adding a variant
    /// here is a deliberate account-layout change rather than something
    /// that can happen by accident.
    pub const COUNT: usize = 7;

    /// Index into a per-role array. `Role` is `#[repr]`-less, so this goes
    /// through an explicit match rather than `as usize`: the compiler then
    /// fails on a new variant instead of silently giving it an index that
    /// collides or runs off the end of a `[_; COUNT]`.
    pub fn index(self) -> usize {
        match self {
            Role::Merchant => 0,
            Role::Arbitrator => 1,
            Role::NodeOperator => 2,
            Role::NotificationProvider => 3,
            Role::OracleProvider => 4,
            Role::RiskIntelligenceProvider => 5,
            Role::SnapshotProvider => 6,
        }
    }
}

/// OFS-4100 §5's 6-category governance taxonomy (OFS-4200 §2).
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace, Debug)]
pub enum ProposalCategory {
    Informational,
    Standards,
    Parameter,
    Treasury,
    ProtocolUpgrade,
    Constitutional,
}

/// Lifecycle state of a `TradeEscrowVault` (OFS-4200 §2, §4).
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace, Debug)]
pub enum VaultState {
    Available,
    Reserved,
    AwaitingFiatSettlement,
    Released,
    Cancelled,
    Frozen,
}

/// A dispute case's resolution outcome (OFS-2400 §17, OFS-4200 §2).
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace, Debug)]
pub enum DisputeOutcome {
    BuyerWins,
    MerchantWins,
    MutualSettlement,
    InvalidDispute,
}
