use anchor_lang::prelude::*;

use crate::{constants::*, error::ErrorCode, state::*};

/// Grows the already-deployed `FeeConfig` by the ten bytes
/// `min_arbitrator_stake_age_secs` (8) and `arbitrator_sortition_bps` (2)
/// added for OFS-4100 §4 and §4.1.
///
/// Both fields were appended after `bump`, so this is a resize and a write
/// of the new tail — no byte before it moves, and every existing decoder
/// keeps reading what it read before. Contrast `staking`'s
/// `migrate_staking_config`, which inserted mid-layout and had to rewrite
/// the whole tail by hand.
///
/// # Both gates come out disabled
///
/// Zero and zero, matching `initialize_fee_config`. The migration
/// deliberately takes no parameters, so it cannot be the thing that
/// switches arbitrator eligibility on.
///
/// This is not a placeholder to tidy up later — it is the only correct
/// value here. Every live stake account's age clock starts at *its* own
/// migration (`staking::migrate_stake_account`), so a 30-day requirement
/// written in this same operation would exclude the entire arbitrator pool
/// for a month, including every honest arbitrator. Governance turns both on
/// through `update_fee_config` once the pool has genuinely aged, which is
/// also the point at which the pool is large enough for a 1/100 draw to
/// leave anybody eligible.
///
/// One-shot for the existing devnet deployment — there is exactly one
/// `FeeConfig`, confirmed against the cluster — and removable once no
/// pre-migration config remains.
#[derive(Accounts)]
pub struct MigrateFeeConfig<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    /// CHECK: still in the pre-migration layout, so it cannot be
    /// deserialized as a `FeeConfig` — Borsh would run off the end of the
    /// buffer looking for the two new fields. Verified three ways before
    /// anything is written: it is the canonical `fee_config` PDA, it is
    /// owned by this program, and it carries the real `FeeConfig`
    /// discriminator.
    #[account(mut, seeds = [FEE_CONFIG_SEED], bump)]
    pub fee_config: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
}

/// Pre-migration length. `FeeConfig` is all fixed-width fields, so this is
/// simply the new size less the ten appended bytes.
const NEW_LEN: usize = 8 + FeeConfig::INIT_SPACE;
const OLD_LEN: usize = NEW_LEN - 10;

/// Trips the build if `FeeConfig` gains or loses a field, because the
/// offsets here would then describe a layout that no longer exists. A
/// silently stale migration would resize by the wrong amount and write the
/// new fields over whatever had taken their place.
/// `OLD_LEN` of 203 is not arithmetic — it is the measured size of the
/// live devnet `FeeConfig` at `6JPUB6RUxgpDPWZqcXhkMYXV8gofMYxwbMfNr8fLAUHX`,
/// read off the cluster before this migration was written. The assertion
/// ties the two together so the derivation cannot drift from the account it
/// has to fit.
const _: () = assert!(
    OLD_LEN == 203,
    "FeeConfig layout changed — revisit migrate_fee_config's byte math \
     against the real deployed account size"
);

pub fn handle_migrate_fee_config(ctx: Context<MigrateFeeConfig>) -> Result<()> {
    let account = ctx.accounts.fee_config.to_account_info();
    require_keys_eq!(*account.owner, crate::ID, ErrorCode::Unauthorized);

    {
        let data = account.try_borrow_data()?;
        require!(data.len() == OLD_LEN, ErrorCode::FeeConfigAlreadyMigrated);
        require!(
            &data[..8] == FeeConfig::DISCRIMINATOR,
            ErrorCode::Unauthorized
        );
    }

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

    // `resize` zero-fills, so both fields are already disabled. Written
    // explicitly anyway: relying on the runtime's fill would make the
    // security-relevant value of these two fields an implicit consequence
    // of an allocator detail rather than a decision in the code.
    let mut data = account.try_borrow_mut_data()?;
    data[OLD_LEN..OLD_LEN + 8].copy_from_slice(&0i64.to_le_bytes());
    data[OLD_LEN + 8..NEW_LEN].copy_from_slice(&0u16.to_le_bytes());
    Ok(())
}
