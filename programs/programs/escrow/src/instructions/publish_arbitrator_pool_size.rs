use anchor_lang::prelude::*;

use crate::arbitration::MIN_DECIDABLE_ARBITRATOR_POOL;
use crate::events::ArbitratorPoolSizePublished;
use crate::{constants::*, error::ErrorCode, state::*};

/// Publishes the count of wallets eligible to arbitrate, creating the
/// singleton [`ArbitrationPolicy`] on first use (OFS-4100 Annex A, option A).
///
/// This is the only writer of the one number the pool floor cannot derive.
/// [`ArbitrationPolicy`]'s own doc has the full argument for why it is a
/// published figure rather than a measured one; what follows is why this
/// instruction is shaped the way it is.
///
/// # `init_if_needed`, deliberately
///
/// Creation and update are one instruction rather than the usual pair. A
/// separate `initialize` would be a step an operator can forget between the
/// program upgrade and the first governance write, and forgetting it looks
/// exactly like a healthy deployment — the floor is simply never in force.
/// Folding them together means the first write brings the account into
/// existence, so there is no state in which governance believes it has
/// published a figure and no account holds one.
///
/// The usual hazard with `init_if_needed` is a re-initialisation that resets
/// state an attacker wanted cleared. It does not apply here: the account is a
/// PDA at a fixed seed with no payload beyond the figure itself, the admin is
/// pinned on creation and checked on every subsequent write, and the only
/// field is one this instruction exists to overwrite.
///
/// # Zero is a legal value and means "stop enforcing the floor"
///
/// Governance can publish zero to switch the floor back off — a deployment
/// that can no longer keep the figure current should do exactly that rather
/// than leave a stale number standing. It is not rejected as a mistake
/// because it is the value every deployment starts at, and because the
/// alternative is an operator with no way back out.
///
/// A figure below [`MIN_DECIDABLE_ARBITRATOR_POOL`] is also legal, and is
/// the honest state of a young network rather than an error. It is reported
/// in [`ArbitratorPoolSizePublished`] alongside the floor so the shortfall is
/// visible, not refused.
#[derive(Accounts)]
pub struct PublishArbitratorPoolSize<'info> {
    #[account(mut, constraint = admin.key() == fee_config.admin @ ErrorCode::Unauthorized)]
    pub admin: Signer<'info>,

    /// The authority of record. Read rather than trusted from the policy
    /// account so that transferring `FeeConfig.admin` moves this power with
    /// it, instead of stranding a second admin nobody remembers setting.
    #[account(seeds = [FEE_CONFIG_SEED], bump = fee_config.bump)]
    pub fee_config: Account<'info, FeeConfig>,

    #[account(
        init_if_needed,
        payer = admin,
        space = 8 + ArbitrationPolicy::INIT_SPACE,
        seeds = [ARBITRATION_POLICY_SEED],
        bump,
    )]
    pub arbitration_policy: Account<'info, ArbitrationPolicy>,

    pub system_program: Program<'info, System>,
}

pub fn handle_publish_arbitrator_pool_size(
    ctx: Context<PublishArbitratorPoolSize>,
    eligible_arbitrators: u32,
) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let policy = &mut ctx.accounts.arbitration_policy;
    let previous = policy.eligible_arbitrators;

    // Written on every call rather than only on creation. On the
    // `init_if_needed` path the account comes back zeroed, so an admin left
    // unset here would be `Pubkey::default()` — a value no signer can ever
    // present, which would be a one-way lock rather than a safe default.
    policy.admin = ctx.accounts.fee_config.admin;
    policy.eligible_arbitrators = eligible_arbitrators;
    policy.updated_at = now;
    policy.bump = ctx.bumps.arbitration_policy;

    emit!(ArbitratorPoolSizePublished {
        admin: ctx.accounts.admin.key(),
        previous,
        eligible_arbitrators,
        min_decidable_pool: MIN_DECIDABLE_ARBITRATOR_POOL,
        timestamp: now,
    });
    Ok(())
}
