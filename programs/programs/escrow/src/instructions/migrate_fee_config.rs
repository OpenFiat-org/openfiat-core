use anchor_lang::prelude::*;

use crate::{constants::*, error::ErrorCode, state::*};

/// Grows the already-deployed `FeeConfig` to the current layout.
///
/// Two tails have now been appended after `bump`, in two separate changes:
///
/// 1. `min_arbitrator_stake_age_secs` (8) + `arbitrator_sortition_bps` (2),
///    for OFS-4100 §4 and §4.1.
/// 2. `settlement_mints` (32 × [`MAX_SETTLEMENT_MINTS`]) +
///    `settlement_mint_count` (1), for the settlement allowlist.
///
/// Every field was appended rather than inserted, so this stays a resize
/// and a write of the new tail — no byte before it moves, and every
/// existing decoder keeps reading what it read before. Contrast `staking`'s
/// `migrate_staking_config`, which inserted mid-layout and had to rewrite
/// the whole tail by hand.
///
/// # It accepts either starting layout
///
/// A live config may be at 203 bytes (never migrated) or 213 (migrated for
/// the arbitrator gates but not the allowlist). Which of the two the devnet
/// singleton is at depends on whether the previous migration was ever
/// actually submitted, and that is a fact about the cluster rather than
/// about this repository — so this reads the account's real length and
/// handles both, rather than encoding a guess that would brick on the other
/// branch. Anything else is refused as already-migrated.
///
/// # What the new tail comes out as
///
/// The two arbitrator gates come out **disabled** (zero and zero), matching
/// `initialize_fee_config`, and the migration takes no parameters so it
/// cannot be the thing that switches arbitrator eligibility on. That is not
/// a placeholder to tidy up later — every live stake account's age clock
/// starts at *its* own migration (`staking::migrate_stake_account`), so a
/// 30-day requirement written in this same operation would exclude the
/// entire arbitrator pool for a month, honest arbitrators included.
///
/// The allowlist comes out **populated**, from
/// [`DEFAULT_SETTLEMENT_MINTS`], and the difference from the gates above is
/// the whole reason it is safe to say so here. Zero is the inert value for
/// a gate; for an allowlist the inert value is a populated list, because an
/// empty one refuses every trade. A migration that left it empty would take
/// a working deployment offline at the moment it ran and keep it offline
/// until a second, separate governance transaction landed.
///
/// That is also why `DEFAULT_SETTLEMENT_MINTS` carries the devnet mint the
/// live fee treasuries are actually denominated in: without it this
/// migration would de-list the running deployment from its own fee
/// collection.
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

/// The layout as originally deployed. Not arithmetic — it is the measured
/// size of the live devnet `FeeConfig` at
/// `6JPUB6RUxgpDPWZqcXhkMYXV8gofMYxwbMfNr8fLAUHX`, read off the cluster
/// before the first migration was written.
const LEN_V1: usize = 203;
/// After the arbitrator gates were appended: `min_arbitrator_stake_age_secs`
/// (8) + `arbitrator_sortition_bps` (2).
const LEN_V2: usize = LEN_V1 + 10;
/// After the settlement allowlist: `settlement_mints` + the count byte.
const NEW_LEN: usize = 8 + FeeConfig::INIT_SPACE;

/// Trips the build if `FeeConfig` gains or loses a field, because the
/// offsets below would then describe a layout that no longer exists. A
/// silently stale migration would resize by the wrong amount and write the
/// new fields over whatever had taken their place. The assertion ties the
/// derived size to the measured one so the two cannot drift apart.
const _: () = assert!(
    NEW_LEN == LEN_V2 + 32 * MAX_SETTLEMENT_MINTS + 1,
    "FeeConfig layout changed — revisit migrate_fee_config's byte math \
     against the real deployed account size"
);

pub fn handle_migrate_fee_config(ctx: Context<MigrateFeeConfig>) -> Result<()> {
    let account = ctx.accounts.fee_config.to_account_info();
    require_keys_eq!(*account.owner, crate::ID, ErrorCode::Unauthorized);

    // Which tails still have to be written is derived from the account's
    // real length rather than assumed, because whether the previous
    // migration was ever submitted is a property of the cluster.
    let old_len = {
        let data = account.try_borrow_data()?;
        let old_len = data.len();
        require!(
            old_len == LEN_V1 || old_len == LEN_V2,
            ErrorCode::FeeConfigAlreadyMigrated
        );
        require!(
            &data[..8] == FeeConfig::DISCRIMINATOR,
            ErrorCode::Unauthorized
        );
        old_len
    };

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

    let mut data = account.try_borrow_mut_data()?;

    if old_len == LEN_V1 {
        // `resize` zero-fills, so both gates are already disabled. Written
        // explicitly anyway: relying on the runtime's fill would make the
        // security-relevant value of these two fields an implicit
        // consequence of an allocator detail rather than a decision in the
        // code.
        data[LEN_V1..LEN_V1 + 8].copy_from_slice(&0i64.to_le_bytes());
        data[LEN_V1 + 8..LEN_V2].copy_from_slice(&0u16.to_le_bytes());
    }

    // The allowlist, unlike the gates, must NOT be left as the zero-fill —
    // see this instruction's doc. Every slot is written, so the padding
    // past the live prefix is explicitly `Pubkey::default()` rather than
    // whatever the resize happened to leave.
    let mut cursor = LEN_V2;
    for slot in 0..MAX_SETTLEMENT_MINTS {
        let mint = DEFAULT_SETTLEMENT_MINTS
            .get(slot)
            .copied()
            .unwrap_or_default();
        data[cursor..cursor + 32].copy_from_slice(mint.as_ref());
        cursor += 32;
    }
    data[cursor] = DEFAULT_SETTLEMENT_MINTS.len() as u8;
    cursor += 1;
    // A balanced walk, for the reason OFS-4200 §7.1 gives and this
    // repository has already been bitten by: an offset computation that
    // skips or double-counts a field produces a well-formed account that is
    // wrong in a way no later read can distinguish from correct.
    require!(cursor == NEW_LEN, ErrorCode::Overflow);
    Ok(())
}
