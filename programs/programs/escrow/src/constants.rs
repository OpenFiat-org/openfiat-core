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
