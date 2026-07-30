use anchor_lang::prelude::*;
use openfiat_programs_shared::Role;

/// Singleton staking configuration (OFS-4200 §5), governance-updatable in
/// a later phase (once `openfiat-governance`'s `update_config_parameter`
/// exists) — for now, updatable only by `admin`.
#[account]
#[derive(InitSpace)]
pub struct StakingConfig {
    pub admin: Pubkey,
    pub mint: Pubkey,
    /// Minimum stake per role, indexed by [`Role::index`].
    ///
    /// This replaced a flat `min_stake` plus a special-cased
    /// `min_stake_arbitrator`. OFS-4100 §7 lists per-role minimums as out
    /// of scope for v1 on the grounds that further differentiation would
    /// be "a future governance parameter change, not new code" — but with
    /// two hardcoded fields that was not true, since there was no per-role
    /// value for governance to change. An array indexed by role is what
    /// actually makes that sentence hold: a new minimum for any role is a
    /// parameter write, and no layout or code change.
    pub min_stake_by_role: [u64; Role::COUNT],
    pub unbonding_period_secs: i64,
    pub slash_bps: u16,
    pub slashing_authority: Pubkey,
    /// Where a `slash`'s forfeited tokens go — not named in OFS-4200 §5,
    /// this workspace's own extension (mirrors `escrow::FeeConfig`'s
    /// treasury-destination pattern rather than inventing a new one).
    pub slash_destination: Pubkey,
    /// Plan decision #4: the RpcConnected/GossipOnly reward asymmetry's
    /// trusted off-chain "reward cranker" — verifies connectivity mode
    /// via gossip-observed `BlockhashAnnounced` history, then calls
    /// `distribute_reward`. Not itself named in OFS-4200 §5.
    pub rewards_authority: Pubkey,
    pub bump: u8,
    pub stake_vault_bump: u8,
    pub rewards_vault_bump: u8,
}

impl StakingConfig {
    /// The minimum this role must hold to count as staked at all.
    pub fn min_stake_for(&self, role: Role) -> u64 {
        self.min_stake_by_role[role.index()]
    }

    /// A stake balance is legal only if it clears the role's minimum or is
    /// zero — "some but not enough" is rejected everywhere it could arise.
    ///
    /// Keeping exit fully open matters: a minimum must never be able to
    /// trap someone's tokens. What it rules out is the silent middle,
    /// where an account still looks staked but no longer qualifies. That
    /// gives every reader of [`StakeAccount::effective_stake`] — governance
    /// vote weight, the escrow dispute tally — the invariant that a
    /// non-zero stake is always a qualifying one.
    pub fn is_legal_balance(&self, role: Role, amount: u64) -> bool {
        amount == 0 || amount >= self.min_stake_for(role)
    }
}

/// One wallet's stake under one role (OFS-4200 §5) — a wallet may hold
/// independent stakes under different roles, each its own PDA.
#[account]
#[derive(InitSpace)]
pub struct StakeAccount {
    pub owner: Pubkey,
    pub role: Role,
    /// Staked and NOT currently unbonding — see [`StakeAccount::effective_stake`].
    pub amount: u64,
    pub unbonding_amount: u64,
    pub unbonding_release_at: i64,
    pub slashed_total: u64,
    /// Plan decision #4 extension — accrued via `distribute_reward`,
    /// paid out via `claim_rewards`. Tracked separately from `amount` so
    /// a reward doesn't silently change a stake's role-eligibility math
    /// until the owner actually claims it.
    pub pending_rewards: u64,
    pub bump: u8,
    /// Unix timestamp at which this account's *current* staked position
    /// began — the anti-grinding clock behind OFS-4100 §4's 30-day
    /// arbitrator stake age. Zero means "holds no stake", not "infinitely
    /// old": every reader must treat zero as failing any age requirement.
    ///
    /// Set when `amount` first goes from zero to positive, cleared when it
    /// returns to zero, and deliberately **not** reset by a later top-up.
    /// Resetting on top-up would punish an honest arbitrator for adding
    /// stake, and would buy nothing: [`StakingConfig::is_legal_balance`]
    /// already forbids holding a balance between zero and the role
    /// minimum, so an account cannot have been aging cheaply at a token
    /// balance and then jump to a qualifying one. The clock always covers
    /// a period during which the full role minimum was locked.
    ///
    /// It sits after `bump` rather than in layout order because appending
    /// is what makes this field's migration safe: every existing decoder —
    /// `crates/rpc::onchain_stake`, both SDKs, the web app — reads fields
    /// at fixed offsets from the start of the account, so a field added at
    /// the end leaves all of them reading exactly what they read before.
    /// Inserting it mid-layout would silently shift `bump` and break every
    /// one of them.
    pub first_staked_at: i64,
}

impl StakeAccount {
    /// How long this account has continuously held its current staked
    /// position, or `None` when it holds no stake (or predates the
    /// [`Self::first_staked_at`] migration and has not been migrated).
    ///
    /// `None` rather than zero seconds so a caller cannot accidentally
    /// compare an unknown age against a threshold and have "unknown" pass
    /// as "brand new but positive". Both cases must fail an age gate, and
    /// making the absent case unrepresentable as a number is what forces
    /// the caller to handle it.
    pub fn stake_age_secs(&self, now: i64) -> Option<i64> {
        if self.amount == 0 || self.first_staked_at == 0 {
            return None;
        }
        Some(now.saturating_sub(self.first_staked_at))
    }

    /// Whether this account has held its stake for at least
    /// `min_age_secs`. A non-positive requirement disables the gate, which
    /// is what makes the parameter safe to ship at zero and raise by
    /// governance later.
    pub fn meets_stake_age(&self, now: i64, min_age_secs: i64) -> bool {
        if min_age_secs <= 0 {
            return true;
        }
        self.stake_age_secs(now)
            .is_some_and(|age| age >= min_age_secs)
    }

    /// OFS-4200 §5's `get_effective_stake` — implemented as a plain
    /// associated function rather than a dispatched CPI instruction.
    /// `openfiat-governance`'s `cast_vote` and `openfiat-escrow`'s
    /// dispute-vote tally (Phase 4b) depend on this crate as a plain
    /// Cargo path dependency and read a `StakeAccount` account directly
    /// (identical to how any Anchor account is deserialized cross-
    /// program) rather than issuing a real CPI call — a same-result,
    /// lower-overhead pattern for a pure read, and the reason no
    /// `get_effective_stake` instruction is dispatched anywhere in this
    /// program's `#[program]` module.
    ///
    /// A balance below the role's minimum counts as **zero**, not as
    /// itself. `stake` and `request_unstake` already refuse to *create*
    /// such a balance ([`StakingConfig::is_legal_balance`]), but `slash`
    /// can land one involuntarily: slashing a 10,000-OPEN arbitrator by
    /// 10% leaves 9,000, which is non-zero and below the minimum. Without
    /// this check that account kept full governance voting weight and full
    /// dispute-tally weight while no longer qualifying for the role at
    /// all — the exact "silent middle" `is_legal_balance` exists to rule
    /// out, reachable through the one path that does not consult it.
    ///
    /// Resolving it here rather than inside `slash` keeps the penalty
    /// proportional. Having `slash` sweep the illegal remainder to zero
    /// would also restore the invariant, but it turns a 10% penalty into
    /// total forfeiture for anyone staked at the minimum — which is the
    /// common case for arbitrators, and flatly contradicts OFS-2400 §16's
    /// "partial, moderate stake slash". The account keeps its tokens and
    /// simply confers no weight until it is topped back up or fully
    /// withdrawn.
    pub fn effective_stake(&self, config: &StakingConfig) -> u64 {
        if self.amount >= config.min_stake_for(self.role) {
            self.amount
        } else {
            0
        }
    }
}
