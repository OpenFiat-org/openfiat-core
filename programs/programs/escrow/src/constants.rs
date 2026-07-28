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
