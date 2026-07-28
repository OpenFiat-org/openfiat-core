use anchor_lang::prelude::*;
use openfiat_programs_shared::{DisputeOutcome, VaultState};

/// Maximum arbitrators one dispute case can seat. Bounds `DisputeCase`'s
/// on-chain size (fixed via `#[max_len]`) — not a spec-mandated number
/// (Chapter 11 §11.9 deliberately keeps the real per-case threshold
/// undisclosed off-chain); this is only the array capacity a case's
/// on-chain commit/reveal bookkeeping can hold.
pub const MAX_ARBITRATORS: usize = 7;

/// A merchant's pooled inventory for one stablecoin (OFS-4200 §4). Backs
/// the Liquidity Vault Architecture (Whitepaper Ch.08) — `reserve_liquidity`
/// only moves counters here; actual token movement happens when a trade
/// escrow is funded from this pool.
#[account]
#[derive(InitSpace)]
pub struct LiquidityVault {
    pub merchant: Pubkey,
    pub mint: Pubkey,
    /// Total tokens ever deposited into this vault's token account.
    pub total: u64,
    /// Reserved against open (not-yet-funded) trade escrows.
    pub reserved: u64,
    /// `total - reserved` minus whatever has already left via
    /// `withdraw_liquidity`/`fund_trade_escrow` — the amount a new
    /// reservation may draw against.
    pub available: u64,
    /// Cumulative amount that has completed settlement (left this vault's
    /// token account via `fund_trade_escrow` and was later `Released`).
    pub settled: u64,
    /// Currently funded into open trade escrows, not yet released or
    /// cancelled back.
    pub pending_settlement: u64,
    pub bump: u8,
    pub token_vault_bump: u8,
}

/// One trade's in-flight escrow (OFS-4200 §4). `reservation_id` is
/// assigned off-chain by `openfiat-core`'s `reservations` crate (OFS-2200)
/// and passed in verbatim — this program never invents its own ID scheme.
#[account]
#[derive(InitSpace)]
pub struct TradeEscrowVault {
    pub reservation_id: u64,
    pub buyer: Pubkey,
    pub seller: Pubkey,
    pub mint: Pubkey,
    pub amount: u64,
    pub state: VaultState,
    /// Set by `approve_settlement` (OFS-2300 §15) — a precondition for
    /// `release_escrow`, kept as its own flag since `VaultState` has no
    /// "Approved" variant (settlement-phase nuance sits a level below
    /// this program's coarser on-chain state machine; the off-chain
    /// `settlement` crate tracks the fuller OFS-2300 §20 state machine).
    pub approved: bool,
    pub created_at: i64,
    pub timeout_at: i64,
    pub bump: u8,
    pub token_vault_bump: u8,
}

/// Singleton fee configuration (OFS-4200 §4), governance-updatable in a
/// later phase (once `openfiat-governance`'s `update_config_parameter`
/// exists) — for now, updatable only by `admin`.
///
/// OFS-4200 names `ad_listing_fee`/`dispute_filing_fee` as flat fees but
/// doesn't specify how `release_escrow`'s own "computes and routes fee
/// splits atomically" actually splits a settlement fee — that detail is
/// left to implementation. `settlement_fee_bps` plus a 4-way basis-point
/// split across the treasuries is this implementation's concrete answer,
/// `[PROPOSED — NEEDS SIGN-OFF]` like every other numeric protocol
/// parameter this workspace has introduced.
#[account]
#[derive(InitSpace)]
pub struct FeeConfig {
    pub admin: Pubkey,
    pub ad_listing_fee: u64,
    pub dispute_filing_fee: u64,
    /// Basis points of a released trade's amount taken as the settlement
    /// fee. `[PROPOSED — NEEDS SIGN-OFF]` default: 15 (0.15%), matching
    /// the taker fee referenced by `openfiat-app`'s own mock governance
    /// data (OFIP-0021).
    pub settlement_fee_bps: u16,
    pub dev_treasury: Pubkey,
    pub ecosystem_treasury: Pubkey,
    pub infra_treasury: Pubkey,
    pub emergency_reserve: Pubkey,
    /// Splits of the settlement fee (not of the trade amount) across the
    /// four destinations above — must sum to `BPS_DENOMINATOR`.
    pub dev_treasury_bps: u16,
    pub ecosystem_treasury_bps: u16,
    pub infra_treasury_bps: u16,
    pub emergency_reserve_bps: u16,
    /// Default payment/review window (OFS-2300 §8a) new trade escrows are
    /// created with.
    pub timeout_secs: i64,
    pub bump: u8,
}

/// On-chain bridge for a trade escrow's dispute (Phase 4b, plan decision
/// #2 — not itself named in OFS-4200 §4). Arbitrator wallets relay the
/// same signed `VoteCommit`/`VoteReveal` events `crates/disputes` already
/// gossips off-chain; `execute_dispute_outcome` tallies what's recorded
/// here itself rather than trusting a caller-supplied outcome.
#[account]
#[derive(InitSpace)]
pub struct DisputeCase {
    pub reservation_id: u64,
    pub trade_escrow: Pubkey,
    pub opened_at: i64,
    pub commit_deadline: i64,
    pub reveal_deadline: i64,
    pub resolved: bool,
    /// Parallel arrays (index i = one arbitrator's slot), rather than a
    /// `Vec<Struct>` — Anchor's `#[max_len]` space accounting is simplest
    /// per-field; a struct-of-arrays costs the same total space.
    #[max_len(MAX_ARBITRATORS)]
    pub arbitrators: Vec<Pubkey>,
    #[max_len(MAX_ARBITRATORS)]
    pub commitments: Vec<[u8; 32]>,
    /// `None` until that seat's `reveal_dispute_vote` has run.
    #[max_len(MAX_ARBITRATORS)]
    pub revealed_outcomes: Vec<Option<DisputeOutcome>>,
    /// Effective stake read at reveal time (0 until revealed) — the
    /// weight `execute_dispute_outcome` tallies with.
    #[max_len(MAX_ARBITRATORS)]
    pub weights: Vec<u64>,
    pub bump: u8,
}
