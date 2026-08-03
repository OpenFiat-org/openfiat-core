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

/// The most seats this case can retire for staying silent.
///
/// Every round but the last can bar a full bench — the last one ends the
/// case either way, so nothing it bars would ever be consulted.
///
/// Composed with [`MIN_ARBITRATORS`] this is also what puts a floor under
/// the arbitrator pool; see
/// [`MIN_DECIDABLE_ARBITRATOR_POOL`](crate::arbitration::MIN_DECIDABLE_ARBITRATOR_POOL)
/// and OFS-4100 Annex A.
pub const MAX_BARRED_ARBITRATORS: usize =
    MAX_ARBITRATORS * (crate::constants::MAX_DISPUTE_ROUNDS as usize - 1);

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

/// Governance's published count of wallets eligible to arbitrate — the one
/// input to the pool floor that the program cannot work out for itself
/// (OFS-4100 Annex A, option A).
///
/// # Why this is a published number and not a derived one
///
/// The floor Annex A asks for is `eligible pool >= MIN_ARBITRATORS + barred
/// so far`. The right-hand side is exact: barring is this program's own
/// bookkeeping, on this program's own account. The left-hand side is the
/// problem, and it is worth being plain about why.
///
/// A Solana program cannot enumerate accounts. There is no way for
/// `execute_dispute_outcome` to count how many wallets hold a qualifying
/// Arbitrator stake, because counting means iterating `openfiat-staking`'s
/// `StakeAccount`s and nothing on chain can do that. Three sources were
/// considered:
///
/// 1. **Derive it from the case's own rounds.** The wallets that have taken
///    a seat here are a *lower* bound on the pool, and a lower bound cannot
///    prove a pool is too small — it is the wrong side of the inequality.
///    Used that way it would end decidable cases, which is a worse bug than
///    the one being fixed. It is still used, but only to *raise* the
///    estimate; see
///    [`PoolFloor::evidenced_pool`](crate::arbitration::PoolFloor::evidenced_pool).
/// 2. **A counter maintained by `openfiat-staking`**, incremented and
///    decremented as arbitrator stakes cross the minimum. This is the
///    correct long-run source: it is exact, it cannot go stale, and no
///    human has to remember to update it. It is not what ships here because
///    it means changing `StakingConfig`'s layout on a live singleton and
///    touching every stake-lifecycle path in another program — a change
///    that belongs to staking, not to this task. The field below is shaped
///    so that counter can replace it later without the floor changing.
/// 3. **A governance attestation** — this account. Weaker than (2) in
///    exactly one way, and the doc for
///    [`Self::eligible_arbitrators`] says which.
///
/// # It ships absent, and absent means the floor is off
///
/// No `initialize_arbitration_policy` has run on any cluster, so on every
/// existing deployment this account does not exist,
/// `execute_dispute_outcome` reads no pool size, and the case bounces to its
/// round budget exactly as it does today. That is the intended default. The
/// floor is an operator tool that governance opts into by publishing a
/// number it stands behind, not a behaviour that appears at upgrade time.
#[account]
#[derive(InitSpace)]
pub struct ArbitrationPolicy {
    /// Pinned to [`FeeConfig::admin`] at creation and checked on every
    /// write, so the pool figure moves through the same authority as every
    /// other arbitration parameter rather than acquiring a second one.
    pub admin: Pubkey,
    /// How many wallets governance asserts currently hold a qualifying
    /// Arbitrator-role stake.
    ///
    /// **Zero means unpublished**, and disables the pool floor outright — it
    /// is not "a pool of zero". A pool genuinely at zero is indistinguishable
    /// from an unmaintained account, and the two must not lead to the same
    /// action when one of them ends live cases early.
    ///
    /// The honest weakness, stated once: this is an assertion, not a
    /// measurement. A value left stale and *high* makes the floor inert,
    /// which is the failure the design is biased toward. A value left stale
    /// and *low* would end a case on the terminal split earlier than the
    /// round budget would have — same payout, same parties, but sooner, and
    /// sooner is what a griefing party wants. That is why
    /// [`PoolFloor::evidenced_pool`](crate::arbitration::PoolFloor::evidenced_pool)
    /// refuses to let this number fall below what the case has actually
    /// seen, and why a deployment that cannot keep it current should leave
    /// it at zero.
    pub eligible_arbitrators: u32,
    /// When [`Self::eligible_arbitrators`] was last written. Carried so an
    /// indexer, an operator or a governance proposal can see the figure's
    /// age — the program does not expire it, because a staleness bound is
    /// itself a parameter nobody has signed off, and silently discarding a
    /// published figure would be a second way to surprise an operator.
    pub updated_at: i64,
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
    /// Seats that committed in an earlier round of this case and never
    /// revealed. They may not take a seat again in this case.
    ///
    /// Without this, the quorum floor is an attack surface rather than a
    /// safeguard. A party who expects to lose takes seats, commits, and
    /// stays silent: fewer than `MIN_ARBITRATORS` reveals is not a
    /// decision, the round re-opens, and repeating it to
    /// [`MAX_DISPUTE_ROUNDS`](crate::constants::MAX_DISPUTE_ROUNDS)
    /// reaches the terminal even split — a guaranteed half of an escrow
    /// they were going to lose entirely. The re-latched seed makes each
    /// round a fresh draw, which is necessary and not sufficient: a stake
    /// large enough to qualify qualifies again.
    ///
    /// So silence costs the seat. An attacker has to win the draw with a
    /// *new* wallet every round, each funding and locking the minimum
    /// stake, and the case runs out of rounds before that becomes cheap.
    ///
    /// Bounded by what the rounds can actually produce: every round but
    /// the last can retire a full bench.
    #[max_len(MAX_BARRED_ARBITRATORS)]
    pub barred: Vec<Pubkey>,
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
    /// How much of this case's deposit the merchant's vault could **not**
    /// cover at open time, and has not been made good since (OFS-4100
    /// §9.3).
    ///
    /// The complement of [`Self::deposit`] against the filing fee that was
    /// configured when the case opened — latched here rather than
    /// recomputed from `FeeConfig` later, because governance can move
    /// `dispute_filing_fee` mid-case and a debt must not change size
    /// because a parameter did.
    ///
    /// `top_up_arbitration_deposit` draws it down as the vault is refilled
    /// — whether from the merchant's own deposit or from
    /// `absorb_stake_recovery` crediting what `openfiat-staking` took out
    /// of their stake. Zero means this case's deposit is whole.
    ///
    /// Placed in declaration order rather than appended, on the same
    /// grounds [`Self::case_seed`] was: no live `DisputeCase` exists to
    /// migrate. Re-verified against devnet immediately before this field
    /// was added — a `getProgramAccounts` sweep of
    /// `HaPpM1QYM3dKp3sX7zhEdft9hB6ncu6xfALAbkyQChQP` returned one
    /// `FeeConfig`, two `LiquidityVault`s and three `TradeEscrowVault`s,
    /// and **no** account carrying `DisputeCase`'s discriminator. A single
    /// pre-existing open case would have made this an appended field or a
    /// migration instead, because everything below it would shift.
    pub deposit_shortfall: u64,
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
    /// How many times a seat has been taken across every round of this case,
    /// counting a wallet once per round it serves.
    ///
    /// Deliberately not deduplicated across rounds. It exists to put a floor
    /// under the pool estimate in
    /// [`PoolFloor::evidenced_pool`](crate::arbitration::PoolFloor::evidenced_pool),
    /// and there it is only ever used to *raise* the estimate — so counting
    /// an honest repeat server twice makes the pool floor harder to trip,
    /// never easier. Deduplicating would need the full history of every
    /// round's bench, which the per-round arrays deliberately discard.
    ///
    /// Placed in declaration order rather than appended, on the same grounds
    /// [`Self::case_seed`] and [`Self::deposit_shortfall`] were, and
    /// re-verified the same way: a `getProgramAccounts` sweep of
    /// `HaPpM1QYM3dKp3sX7zhEdft9hB6ncu6xfALAbkyQChQP` on devnet immediately
    /// before this field was added returned one `FeeConfig`, two
    /// `LiquidityVault`s and three `TradeEscrowVault`s, and **no** account
    /// carrying `DisputeCase`'s discriminator. One live case would have made
    /// this a migration instead.
    pub seats_taken_total: u32,
    /// Why this case ended on the terminal even split, or `None` while it is
    /// unresolved or if it was decided on a verdict (OFS-4100 Annex A).
    ///
    /// The split pays out identically whichever reason applies. It is
    /// recorded because from outside they are otherwise the same event — a
    /// case that quietly bounced to its round budget — and an operator
    /// cannot act on "the arbitrators disagreed" and "there were not enough
    /// arbitrators left to ask" without being able to tell them apart. Also
    /// emitted as [`DisputeTerminalSplit`](crate::events::DisputeTerminalSplit)
    /// so an indexer never has to fetch the account to learn it.
    pub terminal_reason: Option<crate::arbitration::TerminalSplitReason>,
}

/// One merchant's standing debt for arbitration deposits their OPEN
/// liquidity vault could not cover (OFS-4100 §9.3), and the ledger the
/// stake-recovery relay runs against.
///
/// # Why this account exists at all
///
/// §9.3 makes stake the backstop for the arbitration deposit: "a merchant
/// with an empty liquidity vault does not become undisputable". Without
/// something recording *how much* they failed to cover, that sentence has
/// no on-chain referent — `open_dispute_case` computed the shortfall,
/// emitted it in an event, and threw the number away.
///
/// An event is not a proof. `openfiat-staking` cannot read this program's
/// logs, and a relay that told it "this merchant owes 10 OPEN" would be a
/// theft primitive: whoever submits the transaction decides how much stake
/// moves. So the debt is an account, at an address `openfiat-staking`
/// re-derives from the stake account's own owner, and the amount is one it
/// reads rather than one it is handed. See
/// [`staking::escrow_claim`](../../staking/src/escrow_claim.rs) for the
/// reading half.
///
/// # Two monotone counters, one writer each
///
/// [`Self::owed_total`] only ever grows, and only this program writes it.
/// `staking::StakeRecoveryReceipt::recovered_total` only ever grows, and
/// only *that* program writes it. What is still owed is their difference,
/// computed independently by each side from the other's account.
///
/// That is the whole reason no CPI is needed in either direction — OFS-4200
/// §1 forbids `escrow -> staking`, and `staking -> escrow` would close a
/// Cargo dependency cycle besides. Neither program ever writes the other's
/// state; each publishes a counter the other can read.
///
/// [`Self::credited_total`] is this program's own view of that same
/// recovery: how much of what staking has already moved has been turned
/// into vault liquidity by `absorb_stake_recovery`. It lags
/// `recovered_total` between the two transactions and never leads it.
#[account]
#[derive(InitSpace)]
pub struct StakeRecoveryClaim {
    pub merchant: Pubkey,
    /// The OPEN mint the debt is denominated in — the same mint the
    /// deposit was taken in, and the mint `openfiat-staking`'s vault
    /// holds. Pinned into the PDA seeds so a claim under one mint can
    /// never be satisfied out of a stake denominated in another.
    pub mint: Pubkey,
    /// Monotone. `open_dispute_case` adds each case's shortfall as it
    /// opens; nothing ever reduces it, because a debt that shrank when it
    /// was paid would leave the two programs unable to agree on whether it
    /// had been.
    pub owed_total: u64,
    /// Monotone, and never above the staking receipt's `recovered_total`.
    /// `absorb_stake_recovery` advances it as it credits recovered tokens
    /// to the merchant's vault.
    pub credited_total: u64,
    /// How many cases have contributed to [`Self::owed_total`]. Recorded
    /// so an under-funded merchant is visible as a pattern rather than as
    /// a single number that could be one large case or twenty small ones.
    pub case_count: u32,
    pub bump: u8,
}

impl StakeRecoveryClaim {
    /// What `openfiat-staking` has moved but this program has not yet
    /// turned into vault liquidity.
    pub fn absorbable(&self, recovered_total: u64) -> u64 {
        recovered_total.saturating_sub(self.credited_total)
    }
}

/// The allocation must cover what `openfiat-staking`'s decoder reads, or a
/// freshly-created claim would be shorter than its own prefix and every
/// recovery would fail on a length check.
///
/// A `const` assertion rather than a test: both sides are compile-time
/// constants, so this is a build failure at the point of the mistake
/// rather than a red test somebody has to run.
const _: () = assert!(
    8 + StakeRecoveryClaim::INIT_SPACE >= staking::escrow_claim::CLAIM_PREFIX_LEN,
    "StakeRecoveryClaim must be allocated at least as long as the prefix staking decodes"
);

/// The half of the stake-recovery relay that cannot be checked by the
/// compiler on its own.
///
/// `openfiat-staking` reads [`StakeRecoveryClaim`] as raw bytes, because
/// this program already depends on that one and the reverse dependency
/// would be a cycle. Everything it needs to do that — the program id, the
/// seeds, the discriminator, the field offsets — is a constant it declares
/// for itself, and nothing in the type system forces those constants to
/// agree with the account this program actually writes.
///
/// These tests are that force. They live here rather than in `staking`
/// because this side owns the definitions and this side is where a change
/// would originate: whoever renames a field or reorders the struct gets
/// the failure in the file they edited.
///
/// Each failure mode they rule out is silent in production. A wrong
/// program id or seed derives a permanently empty address, so every
/// merchant reads as debt-free. A wrong discriminator makes every claim
/// unreadable, so recovery never runs. Wrong offsets read a plausible
/// number out of the wrong bytes and move stake against it — the only one
/// of the three that loses money rather than failing closed, and the
/// reason the offsets are asserted against a real serialized account
/// rather than by inspection.
#[cfg(test)]
mod stake_recovery_relay_agreement {
    use super::*;
    use anchor_lang::Discriminator;
    use staking::escrow_claim;

    #[test]
    fn staking_holds_this_programs_id() {
        assert_eq!(escrow_claim::ESCROW_PROGRAM_ID, crate::ID);
    }

    #[test]
    fn staking_holds_the_same_seeds() {
        assert_eq!(
            escrow_claim::STAKE_RECOVERY_CLAIM_SEED,
            crate::constants::STAKE_RECOVERY_CLAIM_SEED
        );
        assert_eq!(
            escrow_claim::LIQUIDITY_VAULT_TOKENS_SEED,
            crate::constants::LIQUIDITY_VAULT_TOKENS_SEED
        );
    }

    #[test]
    fn staking_holds_the_same_discriminator() {
        assert_eq!(
            &escrow_claim::STAKE_RECOVERY_CLAIM_DISCRIMINATOR[..],
            StakeRecoveryClaim::DISCRIMINATOR
        );
    }

    /// Serializes a real claim exactly as Anchor stores one, then reads it
    /// back through the decoder `staking` will use on chain.
    #[test]
    fn stakings_decoder_reads_what_this_program_writes() {
        let merchant = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let claim = StakeRecoveryClaim {
            merchant,
            mint,
            owed_total: 7_000_000_009,
            credited_total: 3,
            case_count: 2,
            bump: 254,
        };

        let mut data = StakeRecoveryClaim::DISCRIMINATOR.to_vec();
        anchor_lang::AnchorSerialize::serialize(&claim, &mut data).unwrap();
        assert!(data.len() >= escrow_claim::CLAIM_PREFIX_LEN);

        assert_eq!(
            &data[escrow_claim::CLAIM_MERCHANT_OFFSET..escrow_claim::CLAIM_MERCHANT_OFFSET + 32],
            merchant.as_ref()
        );
        assert_eq!(
            &data[escrow_claim::CLAIM_MINT_OFFSET..escrow_claim::CLAIM_MINT_OFFSET + 32],
            mint.as_ref()
        );
        assert_eq!(
            u64::from_le_bytes(
                data[escrow_claim::CLAIM_OWED_TOTAL_OFFSET..escrow_claim::CLAIM_PREFIX_LEN]
                    .try_into()
                    .unwrap()
            ),
            claim.owed_total
        );
    }
}
