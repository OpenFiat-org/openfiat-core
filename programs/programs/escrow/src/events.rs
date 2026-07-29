//! On-chain event log for everything this program moves (OFS-4100 §9.4).
//!
//! The programs workspace emitted nothing at all before these existed, so
//! a settled trade left only its end state — the fee split, who was paid
//! what, and how a dispute was decided were all unreconstructable once the
//! accounts had moved on. §9.4 treats that as a defect rather than a
//! missing convenience: an unauditable settlement is indistinguishable
//! from an arbitrary one.
//!
//! Each event carries enough to stand alone, so an indexer replaying logs
//! never has to fetch account state to know what happened.

use anchor_lang::prelude::*;
use openfiat_programs_shared::DisputeOutcome;

/// A trade escrow released to the buyer, with the settlement fee's full
/// four-way destination breakdown. Emitted by `release_escrow` and by
/// `execute_dispute_outcome`'s `BuyerWins` path, which release identically.
#[event]
pub struct EscrowReleased {
    pub reservation_id: u64,
    pub buyer: Pubkey,
    pub seller: Pubkey,
    pub mint: Pubkey,
    /// Full escrowed amount, before the fee.
    pub amount: u64,
    /// What the buyer actually received (`amount` minus `fee`).
    pub buyer_amount: u64,
    pub fee: u64,
    pub dev_treasury_amount: u64,
    pub ecosystem_treasury_amount: u64,
    pub infra_treasury_amount: u64,
    /// Includes the basis-point truncation remainder, per OFS-4100 §6.
    pub emergency_reserve_amount: u64,
    /// True when this release came out of an arbitrated dispute rather
    /// than a normal settlement.
    pub via_dispute: bool,
    pub timestamp: i64,
}

/// A trade escrow that ended without a straight release — returned to the
/// seller's liquidity vault, or split. Emitted by `execute_dispute_outcome`.
///
/// `outcome` is `None` for the terminal even split after
/// [`MAX_DISPUTE_ROUNDS`](crate::constants::MAX_DISPUTE_ROUNDS), which is
/// what distinguishes "arbitration decided this" from "arbitration failed
/// and the protocol fell back".
#[event]
pub struct DisputeResolved {
    pub reservation_id: u64,
    pub buyer: Pubkey,
    pub seller: Pubkey,
    pub outcome: Option<DisputeOutcome>,
    /// Which round decided it, from 0.
    pub round: u8,
    /// Total revealed stake behind the winning outcome; 0 when undecided.
    pub winning_weight: u64,
    pub arbitrator_count: u8,
    /// Deposit forfeited into the arbitration pool for the winning
    /// arbitrators, or 0 when it went back to the merchant.
    pub reward_pool: u64,
    /// Deposit returned to the merchant's vault, or 0 when forfeited.
    pub deposit_refunded: u64,
    pub timestamp: i64,
}

/// An arbitration deposit taken from the merchant's OPEN liquidity vault
/// when a case opened. Emitted by `open_dispute_case`.
///
/// `shortfall` is non-zero when the merchant's vault could not cover the
/// configured fee. The case opens regardless — see `open_dispute_case` —
/// so this is the on-chain record of a merchant who was under-funded when
/// disputed.
#[event]
pub struct ArbitrationDepositTaken {
    pub reservation_id: u64,
    pub merchant: Pubkey,
    pub opened_by: Pubkey,
    pub deposit_vault: Pubkey,
    pub mint: Pubkey,
    pub amount: u64,
    pub shortfall: u64,
    pub timestamp: i64,
}

/// One arbitrator's pro-rata share of a forfeited deposit. Emitted by
/// `claim_arbitration_reward`.
#[event]
pub struct ArbitrationRewardClaimed {
    pub reservation_id: u64,
    pub arbitrator: Pubkey,
    /// This arbitrator's revealed stake weight — the numerator of the
    /// pro-rata share.
    pub weight: u64,
    pub winning_weight: u64,
    pub amount: u64,
    pub destination: Pubkey,
    pub timestamp: i64,
}

/// A merchant's advertisement-listing fee, charged against their OPEN
/// liquidity vault. Emitted by `charge_ad_listing_fee`.
#[event]
pub struct AdListingFeeCharged {
    pub merchant: Pubkey,
    pub advertisement_id: [u8; 32],
    pub mint: Pubkey,
    pub amount: u64,
    /// The vault's remaining balance, so an indexer can tell how many more
    /// listings a merchant can currently afford.
    pub vault_available: u64,
    pub timestamp: i64,
}
