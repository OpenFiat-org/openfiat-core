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
