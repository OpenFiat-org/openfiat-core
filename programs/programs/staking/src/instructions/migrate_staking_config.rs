use anchor_lang::prelude::*;
use openfiat_programs_shared::Role;

use crate::{constants::*, error::ErrorCode, state::*};

/// Rewrites an already-deployed `StakingConfig` into the current layout.
///
/// `min_stake` + `min_stake_arbitrator` became `min_stake_by_role`, which
/// grows the account by 40 bytes, so the live singleton can no longer be
/// deserialized as a `StakingConfig` — the account is therefore taken as a
/// raw `AccountInfo` and parsed by hand.
///
/// This exists rather than closing and reinitializing because the config's
/// PDA address, and the two token vaults created alongside it, must not
/// change: the SDKs, the web app and the node all derive that address from
/// the same seed, and `initialize_staking_config` also creates the vaults,
/// so it cannot be re-run against a deployment whose vaults already exist.
///
/// It is a one-shot for the existing devnet deployment and can be dropped
/// once no pre-migration deployment remains — but the mechanism is worth
/// keeping in mind: any future `StakingConfig` layout change needs it.
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

/// Byte offsets in the pre-migration layout:
/// `disc(8) admin(32) mint(32) min_stake(8) min_stake_arbitrator(8) …`
const OLD_ADMIN_OFFSET: usize = 8;
const OLD_MIN_STAKE_OFFSET: usize = 72;
const OLD_MIN_STAKE_ARBITRATOR_OFFSET: usize = 80;
/// Everything from `unbonding_period_secs` onward is unchanged in content;
/// it simply moves 40 bytes later once the array replaces the two u64s.
const OLD_TAIL_OFFSET: usize = 88;
const OLD_LEN: usize = 197;

pub fn handle_migrate_staking_config(
    ctx: Context<MigrateStakingConfig>,
    min_stake_by_role: [u64; Role::COUNT],
) -> Result<()> {
    let account = ctx.accounts.staking_config.to_account_info();
    require_keys_eq!(*account.owner, crate::ID, ErrorCode::Unauthorized);

    // Read every byte we need out before touching the account's size, so
    // the realloc below cannot be reading from a buffer it is resizing.
    let (stored_admin, old_min_stake, old_min_stake_arbitrator, discriminator, tail) = {
        let data = account.try_borrow_data()?;
        require!(data.len() == OLD_LEN, ErrorCode::AlreadyMigrated);

        let mut admin_bytes = [0u8; 32];
        admin_bytes.copy_from_slice(&data[OLD_ADMIN_OFFSET..OLD_ADMIN_OFFSET + 32]);

        let read_u64 = |at: usize| -> u64 {
            let mut b = [0u8; 8];
            b.copy_from_slice(&data[at..at + 8]);
            u64::from_le_bytes(b)
        };

        let mut discriminator = [0u8; 8];
        discriminator.copy_from_slice(&data[..8]);

        (
            Pubkey::from(admin_bytes),
            read_u64(OLD_MIN_STAKE_OFFSET),
            read_u64(OLD_MIN_STAKE_ARBITRATOR_OFFSET),
            discriminator,
            data[OLD_TAIL_OFFSET..].to_vec(),
        )
    };

    require_keys_eq!(
        stored_admin,
        ctx.accounts.admin.key(),
        ErrorCode::Unauthorized
    );

    // Anything the caller leaves at zero keeps its previous effective
    // value, so a migration that only means to introduce one new role's
    // minimum does not have to restate the others and risk a typo.
    let mut migrated = min_stake_by_role;
    for (index, value) in migrated.iter_mut().enumerate() {
        if *value == 0 {
            *value = if index == Role::Arbitrator.index() {
                old_min_stake_arbitrator
            } else {
                old_min_stake
            };
        }
    }

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
    let mut cursor = OLD_MIN_STAKE_OFFSET;
    for value in migrated {
        data[cursor..cursor + 8].copy_from_slice(&value.to_le_bytes());
        cursor += 8;
    }
    data[cursor..cursor + tail.len()].copy_from_slice(&tail);
    Ok(())
}
