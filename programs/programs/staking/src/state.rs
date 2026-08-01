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
    /// Unbonding period per role, in seconds, indexed by [`Role::index`].
    ///
    /// This replaced a single flat `unbonding_period_secs`, for the same
    /// reason `min_stake_by_role` replaced a flat `min_stake`: OFS-4100 §4
    /// signs off *different* periods per role — 24 hours for a merchant,
    /// 3 days for an arbitrator, 7 days for everyone else — and one field
    /// cannot hold three values however the parameter is described.
    ///
    /// The periods are not arbitrary and the ordering is the argument for
    /// them. Unbonding exists so that misconduct discovered after the fact
    /// still has stake to bite on, so each role's period tracks how long
    /// its misconduct takes to surface. A merchant's failure is visible
    /// within one trade's settlement window, and a long lock on merchant
    /// capital is a direct cost to liquidity that buys nothing. An
    /// arbitrator's misconduct only surfaces when a case is revealed and
    /// tallied, which is why theirs is longer. Everyone else — node
    /// operators, oracles, notification and snapshot providers — is judged
    /// on sustained behaviour observed over time rather than on a single
    /// act, so their stake stays exposed the longest.
    ///
    /// Placed exactly where the flat field was rather than appended, which
    /// shifts every subsequent field by 48 bytes. That is deliberate: a
    /// second "unbonding" value sitting at the end of the struct while a
    /// now-meaningless one sat mid-layout is precisely how two sources of
    /// truth for one parameter come into existence. Nothing outside this
    /// workspace decodes `StakingConfig` at fixed offsets — the only
    /// hand-rolled decoder in the wider repo, `crates/rpc::onchain_stake`,
    /// reads `StakeAccount` and is untouched — so the shift costs the two
    /// scripts in `scripts/` and nothing else. See
    /// `migrate_staking_config` for how the live singleton is grown.
    pub unbonding_period_secs_by_role: [i64; Role::COUNT],
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

    /// How long this role's stake stays locked after `request_unstake`.
    ///
    /// An accessor rather than a direct index for the same reason
    /// [`Self::min_stake_for`] is one: `Role::index` is the only place
    /// that maps a role to an array position, so a new role added to the
    /// enum fails to compile there instead of silently reading a
    /// neighbouring role's period here.
    pub fn unbonding_period_for(&self, role: Role) -> i64 {
        self.unbonding_period_secs_by_role[role.index()]
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

/// How much of one merchant's stake has been taken to satisfy
/// `openfiat-escrow`'s arbitration-deposit claim against them (OFS-4100
/// §9.3).
///
/// # Why this is its own account and not a field on `StakeAccount`
///
/// Seven `StakeAccount`s are live on devnet. Widening a deployed
/// `#[account]` shifts every field after the insertion point and leaves
/// each of them undeserializable by both this program and every external
/// decoder, which is a migration — `migrate_stake_account` exists because
/// of exactly that, for exactly one field. A separate account costs a PDA
/// and no migration at all, and it is also the more honest shape: a
/// recovery is a fact about a *debt*, which most stake accounts will never
/// have, rather than a property of staking.
///
/// It is keyed by wallet rather than by `(wallet, role)` because the debt
/// is the merchant's, not a role's. Recovery draws only on the Merchant
/// role's stake — that is the stake OFS-4100 §9.3 calls the backstop, and
/// a node operator's bond is not collateral for someone else's dispute —
/// but the ledger of what has been taken belongs to the wallet.
///
/// # Monotone, and paired
///
/// [`Self::recovered_total`] only grows. `escrow`'s claim records what is
/// owed and also only grows. What is still outstanding is the difference,
/// and each program computes it from the other's account without either
/// ever writing the other's state — which is what lets OFS-4200 §1's ban
/// on an `escrow -> staking` CPI hold without leaving the two sides unable
/// to agree.
#[account]
#[derive(InitSpace)]
pub struct StakeRecoveryReceipt {
    pub merchant: Pubkey,
    /// Monotone total moved out of this wallet's Merchant stake and into
    /// their OPEN liquidity vault against `escrow`'s claim.
    pub recovered_total: u64,
    /// How many separate recoveries produced that total. A claim that took
    /// several passes is one whose stake did not cover it in one go, and
    /// that is worth being able to see without replaying every event.
    pub recovery_count: u32,
    pub bump: u8,
}

impl StakeRecoveryReceipt {
    /// Reads [`Self::recovered_total`] out of an account that may not
    /// exist yet, returning zero when it does not.
    ///
    /// Hand-decoded because the caller — `withdraw_unstaked` — has to
    /// accept a possibly-absent account, and Anchor's way of expressing
    /// that is an optional account the caller may omit. Omitting this one
    /// would read as "fully recovered" and open the gate it exists to
    /// close, so the account stays required and the absence is handled
    /// here instead.
    ///
    /// Every deviation other than absence is an error. An account at this
    /// address that exists but is owned by something else, is too short,
    /// or carries another type's discriminator is not a receipt this
    /// program wrote, and guessing a balance out of it would be the one
    /// mistake that matters: the number gates whether stake may leave.
    pub fn recovered_total_of(info: &AccountInfo, merchant: &Pubkey) -> Result<u64> {
        if info.data_is_empty() {
            return Ok(0);
        }
        require_keys_eq!(
            *info.owner,
            crate::ID,
            crate::error::ErrorCode::NotARecoveryReceipt
        );

        let data = info.try_borrow_data()?;
        // discriminator + merchant + recovered_total
        const PREFIX_LEN: usize = 8 + 32 + 8;
        require!(
            data.len() >= PREFIX_LEN,
            crate::error::ErrorCode::NotARecoveryReceipt
        );
        require!(
            data[..8] == *StakeRecoveryReceipt::DISCRIMINATOR,
            crate::error::ErrorCode::NotARecoveryReceipt
        );
        let found = Pubkey::new_from_array(
            data[8..40]
                .try_into()
                .map_err(|_| error!(crate::error::ErrorCode::NotARecoveryReceipt))?,
        );
        require_keys_eq!(
            found,
            *merchant,
            crate::error::ErrorCode::NotARecoveryReceipt
        );
        Ok(u64::from_le_bytes(
            data[40..PREFIX_LEN]
                .try_into()
                .map_err(|_| error!(crate::error::ErrorCode::NotARecoveryReceipt))?,
        ))
    }
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
