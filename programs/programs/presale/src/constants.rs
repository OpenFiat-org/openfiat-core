use anchor_lang::prelude::*;

/// PDA seed for the singleton `SaleConfig` account (OFS-4200 §3).
#[constant]
pub const SALE_CONFIG_SEED: &[u8] = b"sale_config";

/// PDA seed for the OPEN token vault holding the Community Presale
/// allocation bucket (owner set at genesis — see ../scripts/genesis.ts).
#[constant]
pub const PRESALE_VAULT_SEED: &[u8] = b"presale_vault";

/// PDA seed for the USDC escrow vault that holds contributions until
/// `finalize_sale` sweeps them to the treasury (or `refund` returns them).
#[constant]
pub const SALE_USDC_VAULT_SEED: &[u8] = b"sale_usdc_vault";

/// PDA seed for a per-buyer `Contribution` record: [SEED, sale_config, buyer].
#[constant]
pub const CONTRIBUTION_SEED: &[u8] = b"contribution";

/// Upper bound on the stablecoin whitelist length (OFS-4100 §3) — fixed so
/// `SaleConfig`'s space can be computed at compile time via `InitSpace`.
/// Not `#[constant]` (IDL-exposed): the `#[constant]` macro in this
/// anchor-lang version doesn't support `usize`.
pub const MAX_STABLECOINS: usize = 5;

/// Basis-points denominator (10_000 = 100%).
#[constant]
pub const BPS_DENOMINATOR: u64 = 10_000;

/// The wrapped-SOL mint — a fixed, well-known SPL constant (not Jupiter- or
/// cluster-specific), used so `contribute_with_swap` accepts native SOL
/// (wrapped by the client beforehand) without needing it on the stablecoin
/// whitelist.
pub const WSOL_MINT: Pubkey = pubkey!("So11111111111111111111111111111111111111112");

/// Jupiter Aggregator v6's real, independently-verified program id
/// (<https://solscan.io/account/JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4>).
/// Not referenced directly by program logic — `SaleConfig.swap_program` is
/// the enforced value, set at `initialize_sale` — this constant exists so
/// devnet/mainnet initialization scripts have one canonical source instead
/// of a second hand-copied literal.
pub const JUPITER_V6_PROGRAM_ID: Pubkey = pubkey!("JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4");
