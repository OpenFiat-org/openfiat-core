//! Reading `openfiat-escrow`'s arbitration-deposit debt from inside this
//! program (OFS-4100 §9.3, OFS-4200 §1).
//!
//! # Why this module is hand-rolled
//!
//! `escrow` depends on `staking` — it reads `StakeAccount` to weigh
//! dispute votes — so `staking` cannot depend on `escrow` without closing
//! a Cargo cycle. The type is therefore not in scope here and never can
//! be, which leaves two honest options: put the struct in
//! `openfiat-programs-shared`, or read the bytes.
//!
//! Reading the bytes wins because only one program is on this side of the
//! relationship. The shared crate exists to stop *four* programs each
//! writing their own ban-list gate; there is no second reader of a
//! `StakeRecoveryClaim` to keep in step, so moving the struct there would
//! buy nothing and would put an account definition in a crate that owns no
//! accounts.
//!
//! # What keeps the two sides in step
//!
//! Not discipline. `escrow` asserts at compile time that every constant
//! below matches the account it actually writes — the discriminator, the
//! program id, the seed, and the field offsets — in
//! `escrow::state`'s own test module. Those tests fail loudly if either
//! side moves, which is the property that matters: a decoder that drifts
//! silently is worse than no decoder, because it reads a plausible number
//! out of the wrong bytes and moves stake against it.
//!
//! Only the fields ahead of any variable-length data are read, and only
//! the prefix is length-checked, so `escrow` may append fields to
//! `StakeRecoveryClaim` without touching this file. It may not reorder
//! them.

use anchor_lang::prelude::*;

use crate::error::ErrorCode;

/// `openfiat-escrow`'s program id, as a constant this program can pin
/// `seeds::program` and an account owner against without a Cargo
/// dependency on it (see this module's own doc for why there cannot be
/// one).
///
/// A second copy of the id in `escrow::declare_id!`, and `escrow` asserts
/// the two agree at compile time. If they diverged, every claim lookup
/// would derive an address under a program that writes no claims — so
/// every merchant would read as debt-free and the backstop would silently
/// stop existing.
pub const ESCROW_PROGRAM_ID: Pubkey = pubkey!("HaPpM1QYM3dKp3sX7zhEdft9hB6ncu6xfALAbkyQChQP");

/// PDA seed for an `escrow::StakeRecoveryClaim`: `[SEED, merchant, mint]`.
///
/// Half of what makes the lookup sound, for the same reason
/// `openfiat_programs_shared::BAN_SEED` is: a seed that disagreed with the
/// writer's would derive a permanently empty address, and an empty claim
/// reads as "owes nothing".
pub const STAKE_RECOVERY_CLAIM_SEED: &[u8] = b"stake_recovery_claim";

/// PDA seed for the SPL token account an `escrow::LiquidityVault` owns:
/// `[SEED, merchant, mint]`.
///
/// Needed here because recovered stake is paid into the merchant's own
/// OPEN vault, and the only way to make that destination un-choosable by
/// the caller is to derive it rather than accept it. Like the two above,
/// `escrow` asserts this matches its own constant at compile time — a
/// seed that drifted would derive an address with no token account at it,
/// and recovery would simply stop working rather than pay the wrong party.
pub const LIQUIDITY_VAULT_TOKENS_SEED: &[u8] = b"liquidity_vault_tokens";

/// Anchor's 8-byte discriminator for `escrow::StakeRecoveryClaim` —
/// `sha256("account:StakeRecoveryClaim")[..8]`.
///
/// Checked rather than skipped so that an escrow-owned account of some
/// *other* type at this address cannot be decoded as a claim. That cannot
/// happen through the seeds alone today, but "the seeds make it
/// impossible" is an argument about a different program's PDA layout, and
/// this program should not depend on one.
pub const STAKE_RECOVERY_CLAIM_DISCRIMINATOR: [u8; 8] = [209, 252, 118, 180, 132, 41, 13, 159];

/// Byte offset of `merchant` — immediately after the discriminator.
pub const CLAIM_MERCHANT_OFFSET: usize = 8;
/// Byte offset of `mint`.
pub const CLAIM_MINT_OFFSET: usize = CLAIM_MERCHANT_OFFSET + 32;
/// Byte offset of `owed_total`.
pub const CLAIM_OWED_TOTAL_OFFSET: usize = CLAIM_MINT_OFFSET + 32;
/// The prefix this module reads. Everything after it — `credited_total`,
/// `case_count`, `bump`, and anything `escrow` appends later — is none of
/// this program's business.
pub const CLAIM_PREFIX_LEN: usize = CLAIM_OWED_TOTAL_OFFSET + 8;

/// How much a merchant owes in arbitration deposits their vault could not
/// cover, as read off `escrow`'s own account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StakeRecoveryClaimView {
    pub merchant: Pubkey,
    pub mint: Pubkey,
    /// Monotone: the sum of every case's shortfall, never reduced by
    /// payment. What is still outstanding is this minus the amount
    /// [`crate::state::StakeRecoveryReceipt`] records as already taken.
    pub owed_total: u64,
}

/// Reads the claim at `info`, or `Ok(None)` when the merchant has never
/// been short.
///
/// # This is only half of the check
///
/// The other half belongs to the `#[account]` constraint at every call
/// site, and it is the half that makes this sound:
///
/// ```ignore
/// #[account(
///     seeds = [escrow_claim::STAKE_RECOVERY_CLAIM_SEED, owner.key().as_ref(), mint.key().as_ref()],
///     bump,
///     seeds::program = escrow_claim::ESCROW_PROGRAM_ID,
/// )]
/// pub recovery_claim: UncheckedAccount<'info>,
/// ```
///
/// Anchor re-derives that address from the stake account's *own* owner and
/// rejects the instruction if the account passed is not it. Without that,
/// a caller would hand in some other merchant's claim — or, worse, their
/// own claim while recovering against somebody else's stake. This function
/// only classifies the account they were forced to bring; it cannot tell
/// whose it should have been.
///
/// # Why absence is not an error
///
/// A merchant who has never been disputed while under-funded has no claim
/// account: zero lamports, zero data, owned by the system program. That is
/// the overwhelmingly common case and it means "owes nothing", exactly as
/// a missing `BanRecord` means "not banned". Treating it as an error would
/// make every honest merchant carry an account they have no reason to.
///
/// Every *other* deviation is an error rather than a zero, because at that
/// point the account exists and does not say what it should — an
/// escrow-owned account that fails the discriminator check, or a claim
/// belonging to a different merchant, is a sign the caller assembled the
/// transaction wrongly or deliberately, and either way reading a balance
/// out of it would be guessing.
pub fn read_stake_recovery_claim(
    info: &AccountInfo,
    merchant: &Pubkey,
    mint: &Pubkey,
) -> Result<Option<StakeRecoveryClaimView>> {
    if info.data_is_empty() {
        return Ok(None);
    }
    require_keys_eq!(*info.owner, ESCROW_PROGRAM_ID, ErrorCode::NotARecoveryClaim);

    let data = info.try_borrow_data()?;
    require!(data.len() >= CLAIM_PREFIX_LEN, ErrorCode::NotARecoveryClaim);
    require!(
        data[..8] == STAKE_RECOVERY_CLAIM_DISCRIMINATOR,
        ErrorCode::NotARecoveryClaim
    );

    let view = StakeRecoveryClaimView {
        merchant: Pubkey::new_from_array(
            data[CLAIM_MERCHANT_OFFSET..CLAIM_MERCHANT_OFFSET + 32]
                .try_into()
                .map_err(|_| error!(ErrorCode::NotARecoveryClaim))?,
        ),
        mint: Pubkey::new_from_array(
            data[CLAIM_MINT_OFFSET..CLAIM_MINT_OFFSET + 32]
                .try_into()
                .map_err(|_| error!(ErrorCode::NotARecoveryClaim))?,
        ),
        owed_total: u64::from_le_bytes(
            data[CLAIM_OWED_TOTAL_OFFSET..CLAIM_PREFIX_LEN]
                .try_into()
                .map_err(|_| error!(ErrorCode::NotARecoveryClaim))?,
        ),
    };

    require_keys_eq!(view.merchant, *merchant, ErrorCode::NotARecoveryClaim);
    require_keys_eq!(view.mint, *mint, ErrorCode::NotARecoveryClaim);
    Ok(Some(view))
}

/// What a merchant still owes: `owed_total` less what this program has
/// already taken out of their stake.
///
/// `saturating_sub` rather than a checked one because the two counters are
/// written by two different programs and this must never be the thing that
/// fails. If a receipt somehow ran ahead of a claim, the right reading is
/// "nothing outstanding" — the direction that lets stake move — not an
/// instruction that can no longer be called.
pub fn outstanding(owed_total: u64, recovered_total: u64) -> u64 {
    owed_total.saturating_sub(recovered_total)
}
