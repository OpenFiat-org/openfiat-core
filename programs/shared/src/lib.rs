//! Shared enums (OFS-4200 §2) reused by `escrow`, `staking`, and
//! `governance` so the three programs don't each define their own
//! incompatible copy of the same protocol concept.
//!
//! Also home to the ban-list gate (OFS-7100 §12), for a structural
//! reason: the ban records are owned by `governance`, but the gate has
//! to be enforced from `escrow`, `staking` and `presale` too. Those
//! programs cannot simply depend on `governance` — `governance` already
//! depends on `staking` (it reads `StakeAccount` to weigh votes), so
//! `staking -> governance` would close a dependency cycle and fail to
//! build. Since the gate is *proof of non-existence*, no program ever
//! deserializes a `BanRecord`; all it needs is the seed and the owning
//! program's id, both of which are plain constants. Putting them here
//! breaks the cycle and, more usefully, means all four programs enforce
//! the check with one shared implementation rather than four
//! hand-copied ones that could drift apart.

use anchor_lang::prelude::*;

pub mod sortition;
pub mod token_dispatch;

/// A staked/bonded protocol role (OFS-4200 §2). `staking::StakeAccount`
/// is keyed by `(owner, role)` — one wallet may hold independent stakes
/// under different roles.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace, Debug)]
pub enum Role {
    Merchant,
    Arbitrator,
    NodeOperator,
    NotificationProvider,
    OracleProvider,
    RiskIntelligenceProvider,
    SnapshotProvider,
}

impl Role {
    /// Number of variants. Used as the length of per-role parameter arrays
    /// (`staking::StakingConfig::min_stake_by_role`), so adding a variant
    /// here is a deliberate account-layout change rather than something
    /// that can happen by accident.
    pub const COUNT: usize = 7;

    /// Index into a per-role array. `Role` is `#[repr]`-less, so this goes
    /// through an explicit match rather than `as usize`: the compiler then
    /// fails on a new variant instead of silently giving it an index that
    /// collides or runs off the end of a `[_; COUNT]`.
    pub fn index(self) -> usize {
        match self {
            Role::Merchant => 0,
            Role::Arbitrator => 1,
            Role::NodeOperator => 2,
            Role::NotificationProvider => 3,
            Role::OracleProvider => 4,
            Role::RiskIntelligenceProvider => 5,
            Role::SnapshotProvider => 6,
        }
    }
}

/// OFS-4100 §5's 6-category governance taxonomy (OFS-4200 §2).
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace, Debug)]
pub enum ProposalCategory {
    Informational,
    Standards,
    Parameter,
    Treasury,
    ProtocolUpgrade,
    Constitutional,
}

/// Lifecycle state of a `TradeEscrowVault` (OFS-4200 §2, §4).
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace, Debug)]
pub enum VaultState {
    Available,
    Reserved,
    AwaitingFiatSettlement,
    Released,
    Cancelled,
    Frozen,
}

/// A dispute case's resolution outcome (OFS-2400 §17, OFS-4200 §2).
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace, Debug)]
pub enum DisputeOutcome {
    BuyerWins,
    MerchantWins,
    MutualSettlement,
    InvalidDispute,
}

/// PDA seed for a `governance::BanRecord`: `[BAN_SEED, wallet]`
/// (OFS-7100 §12).
///
/// Deliberately *not* re-declared in each enforcing program. The seed is
/// half of what makes the gate sound — a program that gated on
/// `[b"banned", wallet]` while `governance` listed under `[b"ban",
/// wallet]` would derive an address that is permanently empty and would
/// therefore admit every banned wallet while looking correct.
pub const BAN_SEED: &[u8] = b"ban";

/// `openfiat-governance`'s program id, as a constant the other programs
/// can use for `seeds::program` without taking a Cargo dependency on it
/// (see this module's own doc for why they cannot).
///
/// This is a second copy of the id in `governance::declare_id!`, so
/// `governance` asserts the two agree at compile time — see the
/// `governance_program_id_matches_declare_id` test in that crate. If
/// they ever diverged, every gate would derive a PDA under a program
/// that never writes ban records, and the ban list would silently stop
/// working everywhere at once.
pub const GOVERNANCE_PROGRAM_ID: Pubkey = pubkey!("AVJfKUjHsizkGGUy8sdz4Xma2hVgmgvgg8GmUMs8E4eE");

/// Whether `ban_record` proves its wallet is banned (OFS-7100 §12).
///
/// # The security property
///
/// Enforcement is by **proof of non-existence**, so this function is only
/// half of the check and the *less* important half. The other half —
/// the half that actually makes it sound — belongs to the `#[account]`
/// constraint at every call site:
///
/// ```ignore
/// #[account(
///     seeds = [openfiat_programs_shared::BAN_SEED, owner.key().as_ref()],
///     bump,
///     seeds::program = openfiat_programs_shared::GOVERNANCE_PROGRAM_ID,
/// )]
/// pub ban_record: UncheckedAccount<'info>,
/// ```
///
/// Anchor re-derives that address from the *signer's own key* and rejects
/// the instruction if the account passed does not match it. Without that,
/// a banned caller would simply pass any unrelated empty account and this
/// function would cheerfully report "not banned" — the gate would be
/// decorative. The constraint is what removes the caller's choice of
/// account; this function only classifies the one account they were
/// forced to bring.
///
/// # Why both conditions
///
/// A live ban record is owned by `governance` and holds a discriminator
/// plus fields, so `data_is_empty()` alone is sufficient in practice. The
/// owner check is kept as a second, independent reason for the same
/// conclusion: it means a future `BanRecord` that could legitimately
/// shrink to zero data, or an account left governance-owned by a partial
/// close, still reads as banned instead of silently opening the gate.
/// The two conditions can only disagree in situations where failing
/// closed is the right answer.
///
/// A wallet that has never been listed is presented by the runtime as a
/// non-existent account: zero lamports, zero data, owned by the system
/// program. That is the unbanned case, and it is the default — which is
/// why the gate costs a banned wallet an account they cannot close
/// rather than costing every honest wallet an account they must create.
pub fn wallet_is_banned(ban_record: &AccountInfo) -> bool {
    !ban_record.data_is_empty() || ban_record.owner == &GOVERNANCE_PROGRAM_ID
}
