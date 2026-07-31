use anchor_lang::prelude::*;

use crate::{constants::*, events::EmergencyAuthorityInitialized, state::*};

/// Starts AllenHark's first-year governance exception, and fixes the
/// moment it ends (OFS-4100 §5.1).
///
/// # Why this is permissionless
///
/// It takes no arguments. The holders come from
/// [`ALLENHARK_PRIMARY_HOLDER`]/[`ALLENHARK_SECONDARY_HOLDER`], which are
/// compiled in, and the deadline is `now + FIRST_YEAR_SECS`, which is
/// computed. So the only thing the sender influences is *when* the clock
/// starts — and starting it earlier only ever brings the deadline nearer.
/// There is nothing here for a stranger to gain, and gating it on `admin`
/// would hand `admin` the one lever that matters: never calling it, and
/// so keeping the exception's start date — and with it the freedom to
/// write `vote_lock_secs` — indefinitely ahead of itself.
///
/// # Why it exists alongside `initialize_governance_config`
///
/// A fresh deployment gets this account from
/// `initialize_governance_config`, atomically, so the clock starts at
/// governance genesis exactly as §5.1 describes. The already-deployed
/// devnet config cannot be initialized a second time, so without a
/// standalone path its `EmergencyAuthority` could never be created and
/// `update_governance_config` — which requires it — would be permanently
/// uncallable there. This is that path: a one-shot backfill, callable
/// once on any deployment where the config predates this instruction.
///
/// Both paths use `init` on the same PDA, so whichever runs first wins
/// and the second fails with "already in use". There is no ordering to
/// get wrong and no way to run both.
#[derive(Accounts)]
pub struct InitializeEmergencyAuthority<'info> {
    /// Pays rent. Holds no authority whatsoever — see the doc above.
    #[account(mut)]
    pub payer: Signer<'info>,

    /// Not `mut`, and there is deliberately no instruction anywhere in
    /// this program in which it is: once `init` writes `expires_at`, the
    /// deadline has no writer left. See [`EmergencyAuthority`].
    #[account(
        init,
        payer = payer,
        space = 8 + EmergencyAuthority::INIT_SPACE,
        seeds = [EMERGENCY_AUTHORITY_SEED],
        bump
    )]
    pub emergency_authority: Account<'info, EmergencyAuthority>,

    pub system_program: Program<'info, System>,
}

pub fn handle_initialize_emergency_authority(
    ctx: Context<InitializeEmergencyAuthority>,
) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let emergency_authority = &mut ctx.accounts.emergency_authority;
    write_emergency_authority(emergency_authority, now, ctx.bumps.emergency_authority)?;

    emit!(EmergencyAuthorityInitialized {
        emergency_authority: emergency_authority.key(),
        primary_holder: emergency_authority.primary_holder,
        secondary_holder: emergency_authority.secondary_holder,
        initialized_at: emergency_authority.initialized_at,
        expires_at: emergency_authority.expires_at,
    });
    Ok(())
}

/// The one and only place `expires_at` is ever assigned, shared with
/// `initialize_governance_config` so the two creation paths cannot
/// compute different deadlines.
///
/// Takes no deadline, no duration and no holders — everything it writes
/// is either a compiled-in constant or derived from `now`. A caller has
/// nothing to pass that could lengthen the window.
pub(crate) fn write_emergency_authority(
    emergency_authority: &mut EmergencyAuthority,
    now: i64,
    bump: u8,
) -> Result<()> {
    emergency_authority.primary_holder = ALLENHARK_PRIMARY_HOLDER;
    emergency_authority.secondary_holder = ALLENHARK_SECONDARY_HOLDER;
    emergency_authority.initialized_at = now;
    emergency_authority.expires_at = now
        .checked_add(FIRST_YEAR_SECS)
        .ok_or(crate::error::ErrorCode::Overflow)?;
    emergency_authority.bump = bump;
    Ok(())
}
