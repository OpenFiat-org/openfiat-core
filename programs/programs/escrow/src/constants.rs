use anchor_lang::prelude::*;

/// PDA seed for a `LiquidityVault` data account: `[SEED, merchant, mint]`
/// (OFS-4200 §4).
#[constant]
pub const LIQUIDITY_VAULT_SEED: &[u8] = b"liquidity_vault";

/// PDA seed for the SPL token account a `LiquidityVault` owns. Not named
/// explicitly in OFS-4200 §4 (which specifies the data account's seed
/// only) — this workspace's own extension, kept as a distinct PDA from
/// the data account so the data account can remain a plain Anchor
/// account rather than doubling as a token account.
#[constant]
pub const LIQUIDITY_VAULT_TOKENS_SEED: &[u8] = b"liquidity_vault_tokens";

/// PDA seed for a `TradeEscrowVault` data account:
/// `[SEED, reservation_id.to_le_bytes()]` (OFS-4200 §4).
#[constant]
pub const TRADE_ESCROW_SEED: &[u8] = b"trade_escrow";

/// PDA seed for the SPL token account a `TradeEscrowVault` owns (same
/// extension rationale as `LIQUIDITY_VAULT_TOKENS_SEED`).
#[constant]
pub const TRADE_ESCROW_TOKENS_SEED: &[u8] = b"trade_escrow_tokens";

/// PDA seed for the singleton `FeeConfig` account (OFS-4200 §4).
#[constant]
pub const FEE_CONFIG_SEED: &[u8] = b"fee_config";

/// PDA seed for a `DisputeCase` account: `[SEED, reservation_id.to_le_bytes()]`
/// (Phase 4b, plan decision #2 — not in OFS-4200's own text).
#[constant]
pub const DISPUTE_CASE_SEED: &[u8] = b"dispute_case";

/// PDA seed for the singleton arbitration pool token account.
///
/// Holds OPEN, not the settlement stablecoin, so it cannot share any
/// existing vault — a token account holds exactly one mint. Two things
/// flow through it: arbitration deposits taken from a merchant's vault
/// while a case is open, and — once a case resolves against the merchant
/// — the forfeited deposit, which the arbitrators who decided that case
/// then claim.
///
/// Deliberately distinct from the four settlement-fee treasuries. Those
/// are external wallet-owned accounts routing protocol revenue; this one
/// is program-owned and holds funds the program still owes to a specific
/// merchant or a specific set of arbitrators. Reusing a treasury would
/// mix a liability with revenue.
#[constant]
pub const ARBITRATION_POOL_SEED: &[u8] = b"arbitration_pool";

/// Basis-points denominator (10_000 = 100%), matching `presale`'s constant.
#[constant]
pub const BPS_DENOMINATOR: u64 = 10_000;

/// Default payment-window timeout (OFS-2300 §8a) — governance-configurable
/// via `FeeConfig.timeout_secs`, this is only the `initialize_fee_config`
/// default.
pub const DEFAULT_TIMEOUT_SECS: i64 = 30 * 60;

/// Floor on a dispute's commit and reveal windows.
///
/// `open_dispute_case` is callable by either party, and the opener picks
/// both windows. Unbounded, that is an attack: a one-second commit window
/// closes before any honest arbitrator can see the case, leaving the
/// tally to whoever was ready to transact in the same block.
///
/// This floor is a liveness guard, not the main defence — seat-squatting
/// is what `commit_dispute_vote`'s stake gate exists for. It rules out
/// the degenerate same-block window while staying short enough that the
/// commit/reveal cycle is exercisable in an integration test.
///
/// `[PROPOSED — NEEDS SIGN-OFF]`, and deliberately flagged as low: OFS-2400
/// fixes no numeric window, and a production floor should reflect how long
/// arbitrator discovery actually takes over gossip — realistically minutes
/// to hours, not one minute. Raising it is a constant change here; making
/// it governance-tunable would mean moving it onto `FeeConfig` alongside
/// `timeout_secs`, which is the natural home if this ever needs to move
/// without a redeploy.
pub const MIN_DISPUTE_WINDOW_SECS: i64 = 60;

/// Ceiling on a dispute's commit and reveal windows. The escrow is frozen
/// for the whole of both, so an unbounded window is the mirror attack —
/// parking someone else's funds indefinitely at no cost.
///
/// `[PROPOSED — NEEDS SIGN-OFF]`
pub const MAX_DISPUTE_WINDOW_SECS: i64 = 7 * 24 * 60 * 60;

/// How many arbitration rounds a case gets before the terminal split.
///
/// A round that produces no decisive result must not pay either party —
/// otherwise forcing indecision becomes a way to win (see
/// `execute_dispute_outcome`'s own doc). Instead the case re-opens for a
/// fresh round. This bounds that retry so a case cannot bounce forever,
/// which would freeze the escrow just as effectively as paying nobody.
///
/// `[PROPOSED — NEEDS SIGN-OFF]` — the retry count and the terminal
/// policy it leads to are economic parameters, not derived from any spec.
pub const MAX_DISPUTE_ROUNDS: u8 = 3;

/// The arbitrator stake age OFS-4100 §4 signed off: **30 days**.
///
/// Deliberately *not* what `initialize_fee_config` writes. Both this and
/// the sortition threshold below start at **zero — disabled** on any
/// deployment, and are switched on by governance through
/// `update_fee_config`.
///
/// That is not caution, it is the only correct starting value. A stake age
/// cannot be satisfied by anyone on a chain younger than the requirement:
/// on day one no wallet has held stake for thirty days, so enforcing it at
/// genesis would make every dispute unarbitrable for the network's first
/// month. The same holds for the existing devnet cluster, where every
/// migrated account's clock starts at its migration.
///
/// So these two constants document the values governance should *reach*,
/// and the code ships the values that are true on the day it deploys.
pub const RECOMMENDED_MIN_ARBITRATOR_STAKE_AGE_SECS: i64 = 30 * 24 * 60 * 60;

/// The opening sortition threshold OFS-4100 §4.1 signed off: **100 bps
/// (1/100)**. Ships disabled for the reason above, and additionally
/// because a draw needs a pool to draw from — at 1/100 with ten registered
/// arbitrators the expected number of qualifiers per case is 0.1.
pub const RECOMMENDED_ARBITRATOR_SORTITION_BPS: u16 = 100;
