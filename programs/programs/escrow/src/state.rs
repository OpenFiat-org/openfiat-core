use anchor_lang::prelude::*;
use openfiat_programs_shared::{DisputeOutcome, VaultState};

use crate::constants::MAX_SETTLEMENT_MINTS;

/// Maximum arbitrators one dispute case can seat. Bounds `DisputeCase`'s
/// on-chain size (fixed via `#[max_len]`) — not a spec-mandated number
/// (Chapter 11 §11.9 deliberately keeps the real per-case threshold
/// undisclosed off-chain); this is only the array capacity a case's
/// on-chain commit/reveal bookkeeping can hold.
pub const MAX_ARBITRATORS: usize = 7;

/// Minimum arbitrators whose vote must actually be **counted** before a
/// dispute can be decided.
///
/// Without this the tally decided on whatever revealed, so a single
/// arbitrator could settle a dispute alone — and nothing stopped that being
/// the only one who showed up. Three is the smallest set that yields a real
/// majority and still resolves when one member is absent or dishonest.
///
/// The check deliberately counts votes the tally *uses*, not seats filled.
/// Zero-weight reveals are skipped by `tally`, so counting seats instead
/// would let three zero-stake accounts satisfy the minimum while
/// contributing nothing — reintroducing the seat-squatting shape through the
/// participation check rather than through the weights.
///
/// Falling short is not a verdict. It routes to the same undecided-round
/// path a tie takes: re-arbitrate up to `MAX_DISPUTE_ROUNDS`, then split the
/// escrow evenly, so failing to reach a quorum can never pay either party
/// more than deciding would have.
pub const MIN_ARBITRATORS: usize = 3;

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
    /// fee. Default: **85 (0.85%)** — `[DECISION — protocol steward]`,
    /// superseding the 15 bps this previously carried.
    ///
    /// Borne by the **buyer**, per buy/sell, in the stablecoin being
    /// traded: `release_trade_escrow_funds` deducts it from the escrowed
    /// amount before paying the buyer out, so the merchant receives their
    /// full sale proceeds and the buyer receives `amount - fee`. That is
    /// one of three fees with three different payers — the merchant bears
    /// the ad-listing fee (`charge_ad_listing_fee`) and the arbitration
    /// deposit (`open_dispute_case`); the buyer bears this one.
    ///
    /// Governance-updatable via `update_fee_config`, never a constant.
    /// The four-way split below remains `[PROPOSED — NEEDS SIGN-OFF]`.
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
    /// How long an Arbitrator-role stake must have been held before its
    /// wallet may commit a dispute vote (OFS-4100 §4, signed off at 30
    /// days). Read from [`staking::StakeAccount::first_staked_at`].
    ///
    /// This is dispute *policy*, so it lives here rather than on
    /// `StakingConfig`: staking records the fact of when a position began,
    /// escrow decides how old is old enough to arbitrate. It also means
    /// governance can retune it through the `update_fee_config` path that
    /// already exists, without a second singleton migration.
    ///
    /// Zero disables the age gate, which is what makes it safe to deploy
    /// inert and raise once real arbitrators have accrued age — every
    /// existing stake account's clock starts at its migration, so
    /// switching a 30-day requirement on immediately would lock out the
    /// entire arbitrator pool for a month.
    pub min_arbitrator_stake_age_secs: i64,
    /// Opening sortition threshold in basis points — the share of eligible
    /// arbitrator wallets drawn for a given case at the moment it opens
    /// (OFS-4100 §4.1). Signed off at **100 (1/100)**.
    ///
    /// Widens across the commit window; see
    /// [`openfiat_programs_shared::sortition`] for the schedule and for an
    /// honest account of what the draw does and does not prevent.
    ///
    /// Zero disables sortition entirely. Both this and the age gate above
    /// are appended after `bump` for the same reason
    /// `StakeAccount::first_staked_at` is: every offset before them keeps
    /// its meaning, so `migrate_fee_config` is a resize rather than a
    /// rewrite and no existing decoder shifts.
    pub arbitrator_sortition_bps: u16,
    /// The mints a trade may be escrowed and settled in — the first
    /// [`Self::settlement_mint_count`] entries are live, the rest are
    /// `Pubkey::default()` padding.
    ///
    /// Protocol-steward directive: wSOL, USDC and USDT allowed by default,
    /// governance votes to add more, OPEN once it reaches public sale. See
    /// [`DEFAULT_SETTLEMENT_MINTS`](crate::constants::DEFAULT_SETTLEMENT_MINTS)
    /// for what actually ships and which entries are devnet-only.
    ///
    /// # Why a fixed array rather than a `Vec`
    ///
    /// `#[max_len]` would reserve the same space but write a four-byte
    /// length prefix ahead of the elements, so the tail's meaning would
    /// depend on a value inside it. A fixed array plus a separate count
    /// keeps every byte at a constant offset, which is what makes
    /// `migrate_fee_config` a resize-and-fill rather than a re-encode.
    ///
    /// Appended after `arbitrator_sortition_bps` for the same reason that
    /// field was appended after `bump`: the live singleton migrates by a
    /// resize alone and no existing decoder shifts.
    pub settlement_mints: [Pubkey; MAX_SETTLEMENT_MINTS],
    /// How many of [`Self::settlement_mints`] are in force. Never above
    /// [`MAX_SETTLEMENT_MINTS`]; zero would refuse every settlement, which
    /// `update_fee_config` rejects rather than allowing the protocol to be
    /// switched off by a parameter write.
    pub settlement_mint_count: u8,
}

impl FeeConfig {
    /// Whether `mint` may be escrowed and settled in.
    ///
    /// Reads only the first `settlement_mint_count` entries, so a mint that
    /// was de-listed by shortening the list cannot be matched against stale
    /// bytes left in the tail. It also means `Pubkey::default()` — the
    /// padding value — is never accidentally allowed, which matters because
    /// an unsupplied account defaults to exactly that.
    pub fn allows_settlement_mint(&self, mint: &Pubkey) -> bool {
        let live = (self.settlement_mint_count as usize).min(MAX_SETTLEMENT_MINTS);
        self.settlement_mints[..live].contains(mint)
    }
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
    /// Which arbitration round this case is on, from 0. A round that
    /// reaches no decisive result re-opens the case rather than paying
    /// either party — see `execute_dispute_outcome`. Bounded by
    /// [`MAX_DISPUTE_ROUNDS`](crate::constants::MAX_DISPUTE_ROUNDS).
    pub round: u8,
    /// The windows this case was opened with, retained so a re-opened
    /// round gets the same deadlines the opener originally chose rather
    /// than a fresh, differently-argued pair.
    pub commit_window_secs: i64,
    pub reveal_window_secs: i64,
    /// This round's sortition seed (OFS-4100 §4.1) — the value every
    /// arbitrator's eligibility draw is computed against. Latched from a
    /// recent slot hash when the round opens, and **re-latched on every
    /// re-opened round**: reusing one seed across rounds would mean the
    /// same wallets qualify every time, so an attacker who wins the draw
    /// once wins it for the life of the case.
    ///
    /// Recorded on the account rather than recomputed, so any observer can
    /// reproduce and check every seat's draw from public data. That
    /// verifiability is the point — the program cannot enumerate accounts
    /// to draw names itself, so arbitrators self-select and anyone can
    /// confirm they were entitled to.
    ///
    /// Placed in declaration order rather than appended because, unlike
    /// `FeeConfig`, no live `DisputeCase` exists to migrate — checked
    /// against devnet, which holds a `FeeConfig`, a `LiquidityVault` and
    /// two `TradeEscrowVault`s but no dispute case. A pre-existing open
    /// case would otherwise have needed one, since this sits ahead of the
    /// per-seat vectors.
    pub case_seed: [u8; 32],
    /// When the **current round** opened, as distinct from [`Self::opened_at`],
    /// which records when the case first did.
    ///
    /// The sortition threshold widens across a round's commit window, so it
    /// needs that round's own start. Using `opened_at` would mean a
    /// re-opened round inherited an already-elapsed window and opened its
    /// draw to everyone immediately — silently disabling sortition for
    /// every round after the first, which is exactly the situation an
    /// attacker creates by forcing a re-round. Overwriting `opened_at`
    /// instead would have worked but would have destroyed the case's
    /// original timestamp, which the dispute record is audited against.
    pub round_opened_at: i64,
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
    /// Whether each seat has already drawn its share of the reward. Reset
    /// with the other per-round arrays, so only the round that actually
    /// decided the case can claim (OFS-4100 §9.3).
    #[max_len(MAX_ARBITRATORS)]
    pub reward_claimed: Vec<bool>,
    /// The merchant's OPEN liquidity vault the deposit was taken from, and
    /// where it returns if the merchant is not found at fault.
    ///
    /// The deposit always comes from the merchant, whoever opened the
    /// case. That asymmetry is deliberate: a buyer is often a one-time
    /// participant and must face no cost barrier to raising a dispute,
    /// while the merchant is the party running an ongoing business off an
    /// ongoing vault.
    pub deposit_vault: Pubkey,
    /// The OPEN mint the deposit is denominated in, pinned at open time so
    /// a claim or refund cannot be routed through a different mint.
    pub deposit_mint: Pubkey,
    /// Deposit taken at `open_dispute_case`, per `FeeConfig`.
    ///
    /// Zero when the configured fee was zero **or** the merchant's vault
    /// could not cover it — see `open_dispute_case`, which deliberately
    /// opens the case anyway rather than letting an underfunded merchant
    /// block a buyer's dispute.
    pub deposit: u64,
    /// Set by `execute_dispute_outcome`. `Some` when a round produced a
    /// stake-weighted verdict; stays `None` when the case exhausted its
    /// rounds without deciding — which is what separates a deposit that
    /// was decided against the merchant from one that was never resolved.
    pub outcome: Option<DisputeOutcome>,
    /// Total revealed weight behind [`Self::outcome`] — the denominator
    /// each winning arbitrator's pro-rata share is computed against.
    pub winning_weight: u64,
    /// Forfeited deposit now held in the arbitration pool and claimable by
    /// the arbitrators who decided this case. Zero unless the merchant was
    /// found at fault.
    pub reward_pool: u64,
    /// Of [`Self::reward_pool`], how much is still unclaimed. Tracked so
    /// the final claimant can take the truncation remainder rather than
    /// leaving dust stranded in the pool.
    pub reward_remaining: u64,
    /// Whether the deposit has already been settled — returned to the
    /// merchant or moved into the claimable pool. Guards against a second
    /// settlement if `execute_dispute_outcome` is somehow re-entered.
    pub deposit_settled: bool,
    pub bump: u8,
}
