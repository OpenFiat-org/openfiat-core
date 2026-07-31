use anchor_lang::prelude::*;
use openfiat_programs_shared::Role;

use crate::instructions::shared_logic::require_valid_unbonding_periods;
use crate::{constants::*, error::ErrorCode, state::*};

/// Rewrites an already-deployed `StakingConfig` into the current layout.
///
/// The flat `unbonding_period_secs` became the per-role
/// [`unbonding_period_secs_by_role`](StakingConfig::unbonding_period_secs_by_role)
/// array, which grows the account by 48 bytes, so the live singleton can
/// no longer be deserialized as a `StakingConfig` — the account is
/// therefore taken as a raw `AccountInfo` and parsed by hand.
///
/// This exists rather than closing and reinitializing because the
/// config's PDA address, and the two token vaults created alongside it,
/// must not change: the SDKs, the web app and the node all derive that
/// address from the same seed, and `initialize_staking_config` also
/// creates the vaults, so it cannot be re-run against a deployment whose
/// vaults already exist. Recreating the config would leave every staked
/// token in a vault nothing could sign for.
///
/// # It migrates *layout*, never policy
///
/// The only value it takes is the array that did not exist before. Every
/// other byte — `min_stake_by_role`, `slash_bps`, all three authorities,
/// the bumps — is carried across verbatim. That separation is deliberate:
/// policy belongs to `update_staking_config`, where it is validated,
/// emits [`crate::events::StakingConfigUpdated`], and can be diffed
/// against the account afterwards. A migration that also restated policy
/// would be a parameter write hidden inside a resize, and the one thing
/// nobody could reconstruct afterwards is what the values were before it
/// ran.
///
/// So OFS-4100 §4's rework lands in two steps against the live
/// deployment: this grows the account and expands one unbonding period
/// into seven, then `update_staking_config` writes the 500-OPEN floors
/// and the 5% slash rate.
///
/// # This is the second time this account has been migrated
///
/// The first grew `min_stake` + `min_stake_arbitrator` into
/// `min_stake_by_role`. That path is replaced rather than kept beside
/// this one: the only deployment it targeted has already run it, and a
/// migration accepting two input layouts must *decide* which one it is
/// looking at — for two length-tagged layouts that is one comparison away
/// from rewriting a healthy account with a misparse. [`OLD_LEN`] admits
/// exactly one layout and rejects everything else, including an account
/// that has already been migrated.
///
/// Any future `StakingConfig` layout change replaces the offsets below the
/// same way. The mechanism is worth keeping even while it is idle.
#[derive(Accounts)]
pub struct MigrateStakingConfig<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,

    /// CHECK: parsed by hand below, because it still holds the previous
    /// layout and so cannot be deserialized as a `StakingConfig`. Verified
    /// three ways before anything is written: it is the canonical
    /// `staking_config` PDA, it is owned by this program, and its stored
    /// admin matches the signer.
    #[account(mut, seeds = [STAKING_CONFIG_SEED], bump)]
    pub staking_config: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
}

/// The pre-migration layout, written out as the walk that produces it:
/// `disc(8) admin(32) mint(32) min_stake_by_role(8×COUNT)
///  unbonding_period_secs(8) slash_bps(2) slashing_authority(32)
///  slash_destination(32) rewards_authority(32) bump(1)
///  stake_vault_bump(1) rewards_vault_bump(1)` — 237 bytes at COUNT = 7.
///
/// Composed from the field sizes rather than written as literals so the
/// arithmetic is visible and a reader can balance it, which is the same
/// discipline OFS-4200 §7.1 requires of anything decoding this account
/// off-chain.
const OLD_ADMIN_OFFSET: usize = 8;
const OLD_UNBONDING_OFFSET: usize = 8 + 32 + 32 + 8 * Role::COUNT;
/// Everything from `slash_bps` onward is unchanged in content; it moves 48
/// bytes later once the array replaces the single i64.
const OLD_TAIL_OFFSET: usize = OLD_UNBONDING_OFFSET + 8;
const OLD_LEN: usize = OLD_TAIL_OFFSET + 2 + 32 * 3 + 1 + 1 + 1;

/// The offsets above and `StakingConfig`'s real layout must agree, and
/// this is the one place that can be proved rather than asserted in prose:
/// growing one `i64` into `COUNT` of them is the *only* difference between
/// the two layouts, so the old length plus the added slots has to land
/// exactly on the new one.
///
/// A compile-time check rather than a runtime one because the failure it
/// guards is not a bad input — it is a struct field added, removed or
/// reordered without these constants following. That produces an account
/// that is well-formed and wrong, which is the failure mode a hand-computed
/// offset has already produced once on this exact struct, and the build is
/// the last moment at which it is cheap to catch.
const _: () = assert!(
    OLD_LEN + 8 * (Role::COUNT - 1) == 8 + StakingConfig::INIT_SPACE,
    "MigrateStakingConfig's offsets no longer describe StakingConfig's layout"
);

pub fn handle_migrate_staking_config(
    ctx: Context<MigrateStakingConfig>,
    unbonding_period_secs_by_role: [i64; Role::COUNT],
) -> Result<()> {
    let account = ctx.accounts.staking_config.to_account_info();
    require_keys_eq!(*account.owner, crate::ID, ErrorCode::Unauthorized);

    // Read every byte we need out before touching the account's size, so
    // the resize below cannot be reading from a buffer it is growing.
    let (stored_admin, old_unbonding_period_secs, discriminator, tail) = {
        let data = account.try_borrow_data()?;
        require!(data.len() == OLD_LEN, ErrorCode::AlreadyMigrated);

        let mut admin_bytes = [0u8; 32];
        admin_bytes.copy_from_slice(&data[OLD_ADMIN_OFFSET..OLD_ADMIN_OFFSET + 32]);

        let mut unbonding_bytes = [0u8; 8];
        unbonding_bytes.copy_from_slice(&data[OLD_UNBONDING_OFFSET..OLD_UNBONDING_OFFSET + 8]);

        let mut discriminator = [0u8; 8];
        discriminator.copy_from_slice(&data[..8]);

        (
            Pubkey::from(admin_bytes),
            i64::from_le_bytes(unbonding_bytes),
            discriminator,
            data[OLD_TAIL_OFFSET..].to_vec(),
        )
    };

    require_keys_eq!(
        stored_admin,
        ctx.accounts.admin.key(),
        ErrorCode::Unauthorized
    );

    // Anything the caller leaves at zero inherits the single period the
    // account already had, so a migration that only means to shorten one
    // role's unbonding does not have to restate the other six and risk a
    // typo. Inheriting rather than filling from a constant also means this
    // cannot quietly lengthen a period nobody asked it to touch.
    let mut migrated = unbonding_period_secs_by_role;
    for value in migrated.iter_mut() {
        if *value == 0 {
            *value = old_unbonding_period_secs;
        }
    }
    // Validated after the inheritance fill, against exactly what is about
    // to be written. A deployed config somehow carrying a non-positive
    // period would otherwise be propagated into all seven slots, and every
    // `request_unstake` after it would set a release time already in the
    // past. `initialize_staking_config` has always rejected that, so it
    // should be unreachable — this is what turns "should" into "cannot".
    require_valid_unbonding_periods(&migrated)?;

    let new_len = 8 + StakingConfig::INIT_SPACE;
    let rent = Rent::get()?;
    let extra = rent
        .minimum_balance(new_len)
        .saturating_sub(account.lamports());
    if extra > 0 {
        anchor_lang::system_program::transfer(
            CpiContext::new(
                ctx.accounts.system_program.key(),
                anchor_lang::system_program::Transfer {
                    from: ctx.accounts.admin.to_account_info(),
                    to: account.clone(),
                },
            ),
            extra,
        )?;
    }
    account.resize(new_len)?;

    let mut data = account.try_borrow_mut_data()?;
    data[..8].copy_from_slice(&discriminator);
    let mut cursor = OLD_UNBONDING_OFFSET;
    for value in migrated {
        data[cursor..cursor + 8].copy_from_slice(&value.to_le_bytes());
        cursor += 8;
    }
    data[cursor..cursor + tail.len()].copy_from_slice(&tail);
    Ok(())
}
