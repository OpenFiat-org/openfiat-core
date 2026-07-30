use anchor_lang::prelude::*;

use crate::{constants::*, error::ErrorCode, events::StakeAccountMigrated, state::*};

/// Grows an already-deployed `StakeAccount` by the eight bytes
/// [`StakeAccount::first_staked_at`] added for OFS-4100 §4's arbitrator
/// stake-age requirement.
///
/// The field was appended after `bump` rather than placed in layout order
/// precisely so this migration is a resize and nothing else: the first 82
/// bytes keep their exact meaning, so no byte has to be moved and no
/// existing decoder changes. Contrast `migrate_staking_config`, which
/// inserted mid-layout and therefore had to read, resize, and rewrite the
/// whole tail by hand.
///
/// # Why the clock starts now, not at deployment
///
/// A migrated account gets `first_staked_at = <now>` when it holds stake,
/// and zero when it does not. The alternatives are both wrong:
///
/// - **Treating the absent value as "infinitely old"** would hand every
///   pre-existing account instant arbitrator eligibility — including the
///   throwaway wallets this repo's own conformance tests created. The
///   requirement would ship already satisfied by exactly the accounts it
///   is meant to exclude.
/// - **Letting the caller supply the timestamp** would make stake age an
///   admin-settable number, which is a backdoor around the requirement
///   rather than an implementation of it.
///
/// Starting the clock at migration is the strictest honest option: nobody
/// gains age they did not hold stake through, and no existing staker is
/// forced to unstake and lose their tokens' position to start accruing.
/// The cost is that genuine long-term stakers restart at zero, which on a
/// devnet-only deployment whose OPEN mint has no mint authority is a cost
/// nobody actually bears.
///
/// # Why anybody may call it
///
/// Permissionless, and the caller pays the rent top-up for their trouble.
/// It cannot be abused to reset someone's clock because it is one-shot by
/// construction — the length check below only passes against the
/// pre-migration layout, and a migrated account is eight bytes longer
/// forever after. Requiring the owner's signature would instead mean an
/// account whose owner has lost their key can never be read by a client
/// expecting the current layout.
///
/// One-shot for the current deployment, and removable once no
/// pre-migration account remains.
#[derive(Accounts)]
pub struct MigrateStakeAccount<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    /// CHECK: still in the pre-migration layout, so it cannot be
    /// deserialized as a `StakeAccount` — Borsh would run off the end of
    /// the buffer looking for `first_staked_at`. Verified three ways
    /// before anything is written: owned by this program, carrying the
    /// real `StakeAccount` discriminator, and sitting at the canonical
    /// `[STAKE_ACCOUNT_SEED, owner, role]` address for the owner and role
    /// its own bytes claim.
    #[account(mut)]
    pub stake_account: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
}

/// Pre-migration layout:
/// `disc(8) owner(32) role(1) amount(8) unbonding_amount(8)
///  unbonding_release_at(8) slashed_total(8) pending_rewards(8) bump(1)`
const OLD_LEN: usize = 82;
const NEW_LEN: usize = 8 + StakeAccount::INIT_SPACE;

/// Trips the build if `StakeAccount` gains another field, because the
/// offsets below and `OLD_LEN` above would then be describing a layout
/// that no longer exists. A silently stale migration is worse than a
/// missing one: it would resize accounts by the wrong amount and write
/// `first_staked_at` over whatever field had taken its place.
const _: () = assert!(
    NEW_LEN == OLD_LEN + 8,
    "StakeAccount layout changed — revisit migrate_stake_account's byte math"
);

const OWNER_OFFSET: usize = 8;
const ROLE_OFFSET: usize = 40;
const AMOUNT_OFFSET: usize = 41;
/// `first_staked_at` occupies the eight bytes the resize appends.
const FIRST_STAKED_AT_OFFSET: usize = OLD_LEN;

pub fn handle_migrate_stake_account(ctx: Context<MigrateStakeAccount>) -> Result<()> {
    let account = ctx.accounts.stake_account.to_account_info();
    require_keys_eq!(*account.owner, crate::ID, ErrorCode::Unauthorized);

    let (stored_owner, role_byte, amount) = {
        let data = account.try_borrow_data()?;
        require!(
            data.len() == OLD_LEN,
            ErrorCode::StakeAccountAlreadyMigrated
        );
        require!(
            &data[..8] == StakeAccount::DISCRIMINATOR,
            ErrorCode::NotAStakeAccount
        );

        let mut owner_bytes = [0u8; 32];
        owner_bytes.copy_from_slice(&data[OWNER_OFFSET..OWNER_OFFSET + 32]);
        let mut amount_bytes = [0u8; 8];
        amount_bytes.copy_from_slice(&data[AMOUNT_OFFSET..AMOUNT_OFFSET + 8]);

        (
            Pubkey::from(owner_bytes),
            data[ROLE_OFFSET],
            u64::from_le_bytes(amount_bytes),
        )
    };

    // The discriminator proves this is a `StakeAccount`; the seeds prove it
    // is *the* stake account for the owner and role it claims. Without this
    // a caller could pass a copy they had funded themselves at an arbitrary
    // address and have the program bless it as canonical.
    let (expected, _) = Pubkey::find_program_address(
        &[STAKE_ACCOUNT_SEED, stored_owner.as_ref(), &[role_byte]],
        &crate::ID,
    );
    require_keys_eq!(account.key(), expected, ErrorCode::NotAStakeAccount);

    let rent = Rent::get()?;
    let extra = rent
        .minimum_balance(NEW_LEN)
        .saturating_sub(account.lamports());
    if extra > 0 {
        anchor_lang::system_program::transfer(
            CpiContext::new(
                ctx.accounts.system_program.key(),
                anchor_lang::system_program::Transfer {
                    from: ctx.accounts.payer.to_account_info(),
                    to: account.clone(),
                },
            ),
            extra,
        )?;
    }
    account.resize(NEW_LEN)?;

    // An account holding no stake keeps a zero clock, matching what
    // `initialize_stake_account` writes and what `request_unstake` restores
    // on a full exit — the invariant every reader of
    // `StakeAccount::stake_age_secs` relies on.
    let now = Clock::get()?.unix_timestamp;
    let first_staked_at = if amount > 0 { now } else { 0 };

    let mut data = account.try_borrow_mut_data()?;
    data[FIRST_STAKED_AT_OFFSET..FIRST_STAKED_AT_OFFSET + 8]
        .copy_from_slice(&first_staked_at.to_le_bytes());
    drop(data);

    emit!(StakeAccountMigrated {
        stake_account: account.key(),
        owner: stored_owner,
        amount,
        first_staked_at,
        timestamp: now,
    });
    Ok(())
}
