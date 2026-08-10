use anchor_lang::prelude::*;

/// PDA seed for the singleton `StakingConfig` account (OFS-4200 §5).
#[constant]
pub const STAKING_CONFIG_SEED: &[u8] = b"staking_config";

/// PDA seed for the single global token vault holding every role's staked
/// OPEN (unbonding included, until withdrawn). Not named explicitly in
/// OFS-4200 §5 (which specifies `StakeAccount`'s own seed only) — this
/// workspace's own extension, one shared vault rather than one per
/// `StakeAccount` since staked funds are fungible and only ever move
/// under this program's own authority.
#[constant]
pub const STAKE_VAULT_SEED: &[u8] = b"stake_vault";

/// PDA seed for the reward pool token vault (plan decision #4 — the
/// RpcConnected/GossipOnly reward asymmetry's funding source). Fed from
/// the OPEN allocation bucket set aside for node/service-provider
/// incentives at genesis.
#[constant]
pub const REWARDS_VAULT_SEED: &[u8] = b"rewards_vault";

/// PDA seed for a `StakeAccount`: `[SEED, owner, role_as_u8]` (OFS-4200 §5).
#[constant]
pub const STAKE_ACCOUNT_SEED: &[u8] = b"stake";

/// PDA seed for a [`StakeRecoveryReceipt`](crate::state::StakeRecoveryReceipt):
/// `[SEED, merchant]` (OFS-4100 §9.3).
///
/// Keyed by wallet alone, matching the claim it is paired against in
/// `openfiat-escrow` — that account is per `(merchant, mint)` and this one
/// per merchant, which is the same key in practice because a stake vault
/// holds exactly one mint and `staking_config.mint` pins which.
#[constant]
pub const STAKE_RECOVERY_RECEIPT_SEED: &[u8] = b"stake_recovery_receipt";

/// Basis-points denominator (10_000 = 100%), matching every other program
/// in this workspace.
#[constant]
pub const BPS_DENOMINATOR: u64 = 10_000;

/// OPEN's decimal places (OFS-4100 §1, re-baselined 2026-08-09: OPEN moved
/// from 9 to 6 decimals, matching USDC) — the multiplier the figures below
/// are quoted in. Whole-OPEN amounts are what the spec states and what
/// anyone reads; base units are what the account stores.
pub const OPEN_DECIMALS_MULTIPLIER: u64 = 1_000_000;

/// The per-role minimum stakes OFS-4100 §4 signs off, in base units,
/// indexed by [`Role::index`](openfiat_programs_shared::Role::index).
///
/// Re-baselined 2026-08-09: these are **static USD-equivalent amounts**,
/// pinned at the $0.01 OPEN presale price (Node $5,000, Arbitrator and the
/// four provider roles $1,000, Merchant unchanged at $5). They are *not*
/// oracle-tracked — nothing here re-prices as OPEN's market price moves.
/// When the USD target needs to change, governance re-derives the OPEN
/// figure at whatever peg it chooses and pushes it through
/// `update_staking_config`; this constant only supplies the value a fresh
/// deployment (or an out-of-band `apply-devnet-staking-floors.ts` run)
/// initializes with.
///
/// Merchant and Arbitrator are **floors, not flat fees**: §4 scales the
/// requirement above the floor with the position a participant actually
/// takes, and the figure below is only where that scale starts.
///
/// # Why sortition is what makes a per-wallet floor safe at all
///
/// A flat per-wallet minimum used to be the only thing standing between
/// an attacker and every seat on a dispute: seats went to whoever called
/// `commit_dispute_vote` first, so capture cost exactly as many staked
/// wallets as there were seats to fill. Any floor low enough for an
/// honest arbitrator to afford was also low enough to buy a majority
/// outright — a floor could not simply track a USD target under that
/// design, because the number doing the work of deterring capture and
/// the number an ordinary arbitrator could afford were the same number.
///
/// What actually closes that hole is that seat eligibility is now a
/// per-case draw (`openfiat_programs_shared::sortition`, wired into
/// `escrow::commit_dispute_vote`): a wallet qualifies only if a hash of
/// its stake account against the case seed falls under the threshold, so
/// assembling a majority needs *many* aged, funded wallets rather than
/// exactly the number of seats. The barrier moved from the size of one
/// stake to the number of independent stakes, which is the thing a
/// per-wallet minimum was never able to price — and it is what makes it
/// safe for the floor below to instead track a USD target (currently
/// 100,000 OPEN, $1,000 as of the 2026-08-09 re-baseline) rather than
/// being tuned as a capture deterrent in its own right.
///
/// These are the values a deployment should hold, not values this program
/// writes on its own: `initialize_staking_config` and
/// `update_staking_config` both take the array as a parameter, because a
/// minimum is a governance-tunable figure and hardcoding it is what made
/// the previous two-field layout unable to express §4 at all.
pub const RECOMMENDED_MIN_STAKE_BY_ROLE: [u64; openfiat_programs_shared::Role::COUNT] = [
    500 * OPEN_DECIMALS_MULTIPLIER, // Merchant — floor, scaling above (unchanged, $5)
    100_000 * OPEN_DECIMALS_MULTIPLIER, // Arbitrator — floor, scaling above ($1,000)
    500_000 * OPEN_DECIMALS_MULTIPLIER, // NodeOperator ($5,000)
    100_000 * OPEN_DECIMALS_MULTIPLIER, // NotificationProvider ($1,000)
    100_000 * OPEN_DECIMALS_MULTIPLIER, // OracleProvider ($1,000)
    100_000 * OPEN_DECIMALS_MULTIPLIER, // RiskIntelligenceProvider ($1,000)
    100_000 * OPEN_DECIMALS_MULTIPLIER, // SnapshotProvider ($1,000)
];

/// The per-role unbonding periods OFS-4100 §4 signs off, in seconds,
/// indexed by [`Role::index`](openfiat_programs_shared::Role::index).
///
/// See [`crate::state::StakingConfig::unbonding_period_secs_by_role`] for
/// why they differ by role rather than being one number.
pub const RECOMMENDED_UNBONDING_PERIOD_SECS_BY_ROLE: [i64; openfiat_programs_shared::Role::COUNT] = [
    24 * 60 * 60,     // Merchant — 24 hours
    3 * 24 * 60 * 60, // Arbitrator — 3 days
    7 * 24 * 60 * 60, // NodeOperator
    7 * 24 * 60 * 60, // NotificationProvider
    7 * 24 * 60 * 60, // OracleProvider
    7 * 24 * 60 * 60, // RiskIntelligenceProvider
    7 * 24 * 60 * 60, // SnapshotProvider
];

/// The slashing penalty OFS-4100 §4 signs off: **500 bps (5%)**,
/// superseding the 10% the deployment was initialized with.
///
/// Halving it is not leniency about the same offence — it keeps the
/// penalty *proportional* to whatever the arbitrator floor happens to be,
/// rather than a flat figure tuned against one particular floor value. At
/// the re-baselined 100,000 OPEN floor, 5% is a real cost — 5,000 OPEN, a
/// far larger absolute penalty than 5% of the old 500 OPEN floor ever was
/// — but it is still partial, not liquidating: OFS-2400 §16 asks for a
/// "partial, moderate stake slash" rather than a figure tuned to make a
/// minimum-staked arbitrator's first mistake terminal, and a percentage
/// bound delivers that at any floor size, small or large. It also matters
/// that
/// [`StakeAccount::effective_stake`](crate::state::StakeAccount::effective_stake)
/// already zeroes the weight of anyone slashed below their role minimum:
/// the real consequence of a slash is losing eligibility, and the
/// basis-point figure is the *financial* component on top of that, not
/// the whole of it.
pub const RECOMMENDED_SLASH_BPS: u16 = 500;
