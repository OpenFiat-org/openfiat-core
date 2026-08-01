use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    transfer_checked, Mint, TokenAccount, TokenInterface, TransferChecked,
};
use openfiat_programs_shared::Role;

use crate::{constants::*, error::ErrorCode, escrow_claim, state::*};

/// Releases an elapsed unbonding cohort to its owner — unless the wallet
/// owes an arbitration deposit its liquidity vault could not cover
/// (OFS-4100 §9.3).
///
/// # The debt gate
///
/// This is the third leg of the stake-recovery relay, and without it the
/// other two are decorative. `recover_stake_shortfall` can reach unbonding
/// tokens, but only while they are still here; once this instruction has
/// run they are the merchant's, in a wallet, and no on-chain mechanism
/// claws them back. A merchant who expects a case to go against them would
/// simply have unstaked, waited out 24 hours, and withdrawn — the exact
/// manoeuvre the backstop exists to make impossible.
///
/// So a Merchant-role withdrawal refuses while anything is outstanding.
/// The claim and receipt accounts are **required**, not optional: an
/// optional account can be omitted, and a gate a caller can decline to
/// bring is not a gate. Both are pinned to addresses derived from the
/// signer's own key, so the caller supplies them without being able to
/// choose them — the same construction the ban-list gate uses.
///
/// # It cannot trap anyone's tokens
///
/// The remedy is permissionless and always available. Anyone — the
/// merchant included — may call `recover_stake_shortfall`, which takes
/// what the stake can cover; after it runs, either the debt is settled and
/// this unblocks, or the stake is empty and there was nothing here to
/// withdraw. There is no state in which a wallet holds withdrawable tokens
/// and cannot reach them by first paying what it owes.
///
/// # Only the Merchant role
///
/// The debt is a merchant's, and §9.3 backs it with "the stake every
/// merchant must post to publish advertisements at all". A node operator's
/// or an oracle's bond secures a different promise to a different set of
/// counterparties, and freezing it over an unrelated dispute would be
/// collateralising one role's conduct with another's capital. The accounts
/// are still passed for every role — Anchor fixes the list per instruction
/// — and simply not consulted.
#[derive(Accounts)]
pub struct WithdrawUnstaked<'info> {
    pub owner: Signer<'info>,

    #[account(mint::token_program = token_program)]
    pub mint: InterfaceAccount<'info, Mint>,

    #[account(
        seeds = [STAKING_CONFIG_SEED],
        bump = staking_config.bump,
        constraint = staking_config.mint == mint.key(),
    )]
    pub staking_config: Account<'info, StakingConfig>,

    #[account(
        mut,
        seeds = [STAKE_ACCOUNT_SEED, owner.key().as_ref(), &[stake_account.role as u8]],
        bump = stake_account.bump,
        has_one = owner,
    )]
    pub stake_account: Account<'info, StakeAccount>,

    #[account(mut, seeds = [STAKE_VAULT_SEED], bump = staking_config.stake_vault_bump)]
    pub stake_vault: InterfaceAccount<'info, TokenAccount>,

    #[account(mut, constraint = to.mint == mint.key())]
    pub to: InterfaceAccount<'info, TokenAccount>,

    /// CHECK: pinned to `openfiat-escrow`'s claim PDA for this signer by
    /// the seeds, then decoded by [`escrow_claim::read_stake_recovery_claim`].
    /// Absent for any wallet that has never been disputed while
    /// under-funded, which is the common case and reads as "owes nothing".
    #[account(
        seeds = [
            escrow_claim::STAKE_RECOVERY_CLAIM_SEED,
            owner.key().as_ref(),
            mint.key().as_ref(),
        ],
        bump,
        seeds::program = escrow_claim::ESCROW_PROGRAM_ID,
    )]
    pub recovery_claim: UncheckedAccount<'info>,

    /// CHECK: this program's own receipt, pinned by the seeds and decoded
    /// by [`StakeRecoveryReceipt::recovered_total_of`]. Unchecked rather
    /// than a typed `Account` because it does not exist until the first
    /// recovery, and a typed account cannot express "may legitimately be
    /// absent" without becoming optional — which would let a caller omit
    /// it and read as fully paid.
    #[account(
        seeds = [STAKE_RECOVERY_RECEIPT_SEED, owner.key().as_ref()],
        bump,
    )]
    pub recovery_receipt: UncheckedAccount<'info>,

    pub token_program: Interface<'info, TokenInterface>,
}

pub fn handle_withdraw_unstaked(ctx: Context<WithdrawUnstaked>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let stake_account = &ctx.accounts.stake_account;
    require!(
        stake_account.unbonding_amount > 0,
        ErrorCode::NoUnbondingBalance
    );
    require!(
        now >= stake_account.unbonding_release_at,
        ErrorCode::StillUnbonding
    );

    if stake_account.role == Role::Merchant {
        let claim = escrow_claim::read_stake_recovery_claim(
            &ctx.accounts.recovery_claim.to_account_info(),
            &ctx.accounts.owner.key(),
            &ctx.accounts.mint.key(),
        )?;
        let owed_total = claim.map(|claim| claim.owed_total).unwrap_or(0);
        let recovered_total = StakeRecoveryReceipt::recovered_total_of(
            &ctx.accounts.recovery_receipt.to_account_info(),
            &ctx.accounts.owner.key(),
        )?;
        require!(
            escrow_claim::outstanding(owed_total, recovered_total) == 0,
            ErrorCode::StakeRecoveryOutstanding
        );
    }

    let amount = stake_account.unbonding_amount;
    let bump = ctx.accounts.staking_config.bump;
    let signer_seeds: &[&[u8]] = &[STAKING_CONFIG_SEED, &[bump]];

    transfer_checked(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: ctx.accounts.stake_vault.to_account_info(),
                mint: ctx.accounts.mint.to_account_info(),
                to: ctx.accounts.to.to_account_info(),
                authority: ctx.accounts.staking_config.to_account_info(),
            },
            &[signer_seeds],
        ),
        amount,
        ctx.accounts.mint.decimals,
    )?;

    ctx.accounts.stake_account.unbonding_amount = 0;
    Ok(())
}
