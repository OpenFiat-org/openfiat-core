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

/// PDA seed for a [`StakeRecoveryClaim`](crate::state::StakeRecoveryClaim):
/// `[SEED, merchant, mint]` (OFS-4100 §9.3).
///
/// Keyed by the merchant's **wallet**, not by a reservation id, and that
/// is load-bearing rather than incidental. `openfiat-staking` has to reach
/// this account knowing nothing but the owner of a stake account it is
/// already holding, exactly as the ban-list gate reaches a `BanRecord`
/// from a signer's key. A per-case seed would leave staking unable to
/// derive an address at all, since it has no way to enumerate a merchant's
/// disputes — and an address the caller supplies is an address the caller
/// chooses.
#[constant]
pub const STAKE_RECOVERY_CLAIM_SEED: &[u8] = b"stake_recovery_claim";

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

/// Capacity of `FeeConfig`'s settlement-mint allowlist.
///
/// Sixteen rather than the three the list ships with, because the whole
/// point of the design is that governance adding a stablecoin is a
/// *parameter write* through `update_fee_config`. Sizing the array to the
/// initial contents would make the fourth mint an account-layout change, a
/// resize migration and a redeploy — which is how a governance-tunable
/// allowlist quietly becomes a hardcoded one.
///
/// It costs 512 bytes of rent on a single global singleton, paid once.
pub const MAX_SETTLEMENT_MINTS: usize = 16;

/// The mints a trade may be escrowed and settled in on day one.
///
/// Protocol-steward directive: "Token mints allowed by default wsol, usdc,
/// usdt. Governance can vote to add more tokens. Open will be supported
/// once it comes to public sale."
///
/// # OPEN is deliberately absent
///
/// Per the directive above. Note this restricts what may be *traded*, not
/// what the protocol charges in: a merchant's OPEN vault — which funds the
/// ad-listing fee and the arbitration deposit — is still creatable, because
/// `create_liquidity_vault` carves it out explicitly. That carve-out is
/// `[PROPOSED — NEEDS SIGN-OFF]`; see that instruction for the argument and
/// for what breaks without it.
///
/// # Three of these four addresses were verified on devnet, not assumed
///
/// Each was read off the cluster before being written here: wSOL,
/// `2bHPi5hA…` and `C4rSGhdx…` are all owned by the legacy SPL Token
/// program, and `SK1JEb…` by Token-2022. That check is the whole point —
/// an allowlist whose entries were taken on trust is a list of addresses
/// somebody typed, and the failure it is supposed to prevent is exactly a
/// mint that looks right and is not.
///
/// # Three of the four entries are DEVNET SCAFFOLDING
///
/// Only wSOL is cluster-independent. The other three are mints this project
/// controls on devnet so that end-to-end tests can actually obtain the
/// tokens they settle in, and **none of them may carry to any other
/// cluster**: on mainnet each would be a look-alike of the asset it is
/// named after, which is the precise failure this allowlist exists to
/// refuse. A mainnet deployment replaces entries 2–4 wholesale.
///
/// The list is also a deliberate mix of both token programs — three legacy
/// SPL and one Token-2022 — so that the allowlist itself demonstrates both
/// dispatch paths are reachable rather than leaving one of them untested.
/// That is the same mistake the pre-migration fixtures made: every fixture
/// mint was Token-2022, so nothing ever exercised the mints the escrow
/// actually needs to hold.
pub const DEFAULT_SETTLEMENT_MINTS: [Pubkey; 4] = [
    // Wrapped SOL. Cluster-independent, and owned by the **legacy** SPL
    // Token program — one of the mints that proved the
    // `Program<'info, Token2022>` constraint could never have held real
    // settlement assets.
    pubkey!("So11111111111111111111111111111111111111112"),
    // DEVNET-ONLY mock USDC. Legacy SPL Token, 6 decimals, faucet mint
    // authority `4oiCmGrMRL4m4RJsRX6F7nCDeEqoiKLYm5hsDcLFvAJB` — a
    // dedicated key, deliberately not the program upgrade authority.
    //
    // Circle's canonical devnet USDC (`4zMMC9srt5Ri…`) was considered and
    // rejected for this slot: nobody here holds its mint authority, so no
    // test can obtain it. An allowlisted mint that no test can acquire is
    // worse than an absent one, because it reads as supported without ever
    // being exercised.
    pubkey!("2bHPi5hA4zrmPAfrvLmEexg3KJjpTjNkUcxWnzUPeRRU"),
    // DEVNET-ONLY mock USDT. Legacy SPL Token, 6 decimals, same faucet
    // authority as the mock USDC above.
    pubkey!("C4rSGhdxWhSFQuFcAxQti1JvBxriwHJoHtJjfhs5p24Y"),
    // DEVNET-ONLY settlement mint, and the one Token-2022 entry. Minted by
    // this repository (`scripts/mint-test-usdc.ts`) and already wired into
    // the live deployment as `feeConfigTreasuryAtas.settlementMint` — all
    // four devnet fee-treasury ATAs are denominated in it, so a list
    // without it would de-list the running deployment from its own fee
    // collection the moment the migration ran.
    pubkey!("SK1JEbfsjjTG2WELNirmM7iJVcdnwerqfF32kCnoWsM"),
];

/// The opening sortition threshold OFS-4100 §4.1 signed off: **100 bps
/// (1/100)**. Ships disabled for the reason above, and additionally
/// because a draw needs a pool to draw from — at 1/100 with ten registered
/// arbitrators the expected number of qualifiers per case is 0.1.
pub const RECOMMENDED_ARBITRATOR_SORTITION_BPS: u16 = 100;
