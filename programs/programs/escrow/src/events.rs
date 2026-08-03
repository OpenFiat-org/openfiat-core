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
    /// Includes the basis-point truncation remainder, per OFS-4100 §6 —
    /// so this is the one share that is not exactly its own bps slice of
    /// `fee`, and an indexer reconciling the four against `fee` should
    /// expect the difference here rather than treating it as an error.
    pub ecosystem_treasury_amount: u64,
    pub infra_treasury_amount: u64,
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

/// Why a dispute case ended on the terminal even split. Emitted by
/// `execute_dispute_outcome` alongside — never instead of —
/// [`DisputeResolved`] (OFS-4100 Annex A, option A).
///
/// A separate event rather than a field on `DisputeResolved` for the same
/// reason [`StakeRecoveryClaimRecorded`] is separate from
/// [`ArbitrationDepositTaken`]: the two answer different questions.
/// `DisputeResolved` says what happened to the money and fires on every
/// resolution; this fires only on the split and says why arbitration did not
/// produce a verdict. An indexer that wants to alert on a failing arbitrator
/// pool subscribes to this one and nothing else.
///
/// Everything below is carried explicitly rather than left to be joined
/// against the account, because the whole point is that an operator can act
/// on the log alone. `reason` distinguishes the failure; the counts are the
/// evidence behind it.
#[event]
pub struct DisputeTerminalSplit {
    pub reservation_id: u64,
    pub reason: crate::arbitration::TerminalSplitReason,
    /// The round the case stopped on, from 0. Below
    /// `MAX_DISPUTE_ROUNDS - 1` only for
    /// [`TerminalSplitReason::PoolExhausted`](crate::arbitration::TerminalSplitReason::PoolExhausted),
    /// which is the one path that stops short of the round budget.
    pub round: u8,
    /// Seats filled in that round.
    pub seats_this_round: u8,
    /// Reveals the tally counted in that round — zero-weight reveals
    /// excluded, exactly as `MIN_ARBITRATORS` is checked.
    pub counted_reveals: u8,
    /// Wallets barred from this case by the time it ended.
    pub barred: u8,
    /// How many times a seat was taken across every round of this case.
    pub seats_taken_total: u32,
    /// How large the eligible pool had to be for another round to be worth
    /// opening: `MIN_ARBITRATORS + barred`.
    pub required_pool: u32,
    /// Governance's published eligible-arbitrator count at that moment.
    /// **Zero means unpublished**, which disables the pool floor — so a
    /// `RoundUnstaffed` alongside a zero here is a network that should
    /// consider publishing one, not a network whose pool is empty.
    pub published_pool: u32,
    pub timestamp: i64,
}

/// Governance published a new eligible-arbitrator count. Emitted by
/// `publish_arbitrator_pool_size`.
///
/// Worth an event of its own because the figure it carries can end live
/// dispute cases early, and a parameter with that reach should leave a
/// record of who moved it and when rather than only its latest value.
#[event]
pub struct ArbitratorPoolSizePublished {
    pub admin: Pubkey,
    pub previous: u32,
    pub eligible_arbitrators: u32,
    /// The floor from OFS-4100 Annex A, carried so a reader can see at a
    /// glance whether the published pool clears it without knowing the
    /// protocol's constants.
    pub min_decidable_pool: u32,
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

/// A merchant's arbitration-deposit debt grew because a case opened
/// against a vault that could not cover the filing fee (OFS-4100 §9.3).
/// Emitted by `open_dispute_case` alongside [`ArbitrationDepositTaken`].
///
/// Separate from that event because the two say different things at
/// different scopes: `ArbitrationDepositTaken` reports one case, this
/// reports the merchant's running position across every case. An indexer
/// answering "is this merchant good for their deposits" wants the second.
#[event]
pub struct StakeRecoveryClaimRecorded {
    pub reservation_id: u64,
    pub merchant: Pubkey,
    pub claim: Pubkey,
    pub mint: Pubkey,
    /// This case's contribution.
    pub shortfall: u64,
    /// The merchant's total debt after it.
    pub owed_total: u64,
    /// Of that total, how much has already been recovered from stake and
    /// credited back — so the outstanding balance is readable from this
    /// event alone.
    pub credited_total: u64,
    pub case_count: u32,
    pub timestamp: i64,
}

/// Stake that `openfiat-staking` moved into a merchant's OPEN liquidity
/// vault has been credited to that vault's counters. Emitted by
/// `absorb_stake_recovery`.
///
/// The tokens arrived in an earlier transaction, in a different program,
/// which is precisely why this event exists: between the two, the vault
/// holds a balance its own accounting does not know about, and only this
/// event says when that stopped being true.
#[event]
pub struct StakeRecoveryAbsorbed {
    pub merchant: Pubkey,
    pub claim: Pubkey,
    pub mint: Pubkey,
    pub amount: u64,
    pub owed_total: u64,
    pub credited_total: u64,
    /// Still owed after this credit. Non-zero means the merchant's stake
    /// has not covered their debt — see `recover_stake_shortfall`.
    pub outstanding: u64,
    pub vault_available: u64,
    pub timestamp: i64,
}

/// An under-funded arbitration deposit was made good out of the merchant's
/// vault. Emitted by `top_up_arbitration_deposit`.
///
/// `remaining_shortfall` is the honest part. A top-up takes whatever the
/// vault can cover and no more, so a partial one is normal and leaves this
/// non-zero — the arbitrators on this case are then owed a reward the
/// merchant has still not funded, and that has to be visible rather than
/// inferred from a deposit that quietly stopped growing.
#[event]
pub struct ArbitrationDepositToppedUp {
    pub reservation_id: u64,
    pub merchant: Pubkey,
    pub mint: Pubkey,
    pub amount: u64,
    /// The case's deposit after this top-up.
    pub deposit: u64,
    pub remaining_shortfall: u64,
    pub vault_available: u64,
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
