use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    transfer_checked, Mint, TokenAccount, TokenInterface, TransferChecked,
};
use openfiat_programs_shared::Role;

use crate::{
    constants::*, error::ErrorCode, escrow_claim, events::StakeShortfallRecovered, state::*,
};

/// Takes a merchant's stake to cover an arbitration deposit their
/// liquidity vault could not (OFS-4100 §9.3).
///
/// # Permissionless, because the alternative is a theft primitive
///
/// The obvious shape for this is a signed relay: an authority watches
/// `openfiat-escrow`, decides a merchant is short, and calls in with the
/// amount. That shape is unacceptable here for the same reason
/// `execute_dispute_outcome` refuses a caller-supplied outcome — a
/// transaction whose *value* is chosen by whoever submits it is a
/// transaction that moves as much stake as the submitter would like it to.
/// The existing `slashing_authority` is not a counter-example: a slash's
/// amount is `slash_bps` of a balance the program reads, so the authority
/// picks the target and never the number.
///
/// So this takes no amount and no authority. It reads
/// `escrow::StakeRecoveryClaim` — at an address Anchor re-derives from
/// this stake account's own owner, under `escrow`'s program id — subtracts
/// what this program has already taken, and moves the difference or
/// whatever is left, whichever is smaller. The caller's entire influence
/// is deciding *when*, and the merchant is the only party who benefits
/// from that being later.
///
/// # Unbonding is drained before the active balance
///
/// This is the ordering the whole design turns on. A merchant's unbonding
/// period is 24 hours (OFS-4100 §4) and a dispute's windows can run to a
/// week each, so a merchant who sees a case opened against an empty vault
/// has ample time to `request_unstake` and walk the stake out from under
/// the debt. Two things stop that:
///
/// 1. The debt exists from the moment the case opens, not from the moment
///    it resolves — see `escrow::open_dispute_case` for why that timing is
///    the only enforceable one.
/// 2. Unbonding tokens have not left. They sit in the same stake vault
///    until `withdraw_unstaked`, so recovery reaches them, and it reaches
///    them *first* — taking the active balance first would let a merchant
///    shelter funds simply by having asked to leave.
///
/// The third leg is `withdraw_unstaked` itself, which refuses while
/// anything is outstanding. Together they mean the only way out is through
/// the debt, and paying it is something anyone can do on the merchant's
/// behalf.
///
/// # When the stake does not cover it
///
/// OFS-4100's status banner leaves this corner open. The answer here is:
/// take everything there is, record the remainder, and never pretend. No
/// pro-rata split across cases and no refusal to act.
///
/// Pro-rata was rejected because there is nothing to divide fairly *here*
/// — the claim is one running total per merchant, and which case a
/// recovered token ends up funding is decided later, by
/// `escrow::top_up_arbitration_deposit`, against each case's own recorded
/// shortfall. Refusing a partial recovery was rejected because it would
/// mean a merchant one lamport short of the full debt keeps all of their
/// stake.
///
/// What the losing side gets instead of a share is the truth: the
/// remainder stays on the claim as a standing debt, [`StakeShortfallRecovered`]
/// carries `outstanding` on every emission, and the stake stays frozen
/// against withdrawal for as long as it is non-zero. `[PROPOSED — NEEDS
/// SIGN-OFF]` — this is the design question §9.3 records as unanswered.
///
/// # This is not a slash
///
/// The tokens go to the merchant's own OPEN liquidity vault, not to a
/// forfeiture destination, and `slashed_total` is untouched. They are
/// collected there rather than sent straight to the arbitration pool
/// because the pool is `escrow`'s, and only `escrow` can decide which case
/// a payment belongs to and adjust that case's accounting to match. This
/// program moves value to a place `escrow` controls and stops; see
/// `escrow::absorb_stake_recovery` for the other half.
/// # Every deserialized account here is boxed, and must stay that way
///
/// This struct's generated `try_accounts` overflowed SBF's 4 KB stack
/// frame — by **eight bytes**, at 4104 of 4096, with ten accounts of which
/// `StakingConfig` alone carries two seven-element arrays and two of the
/// rest are token accounts.
///
/// The failure mode is what makes this worth a comment rather than a
/// commit message. `cargo build-sbf` reports the overflow as a non-fatal
/// `Error:` line, **still emits a binary, and still exits 0**; the binary
/// then dies at runtime with "Access violation ... at address 0x0",
/// attributed to whichever code path happened to be running rather than to
/// the account that pushed the frame over. A local `solana-test-validator`
/// run proves nothing here — it runs the same binary, so a passing suite
/// means the overflow did not happen to corrupt anything on those paths,
/// not that the frame fits.
///
/// It is also toolchain-sensitive: a direct `cargo build-sbf` on the
/// platform-tools version this workspace resolves to emits no diagnostic at
/// all, while `anchor build` does. The check that counts is therefore
/// `anchor build`'s own log, grepped for both wordings the backend uses —
/// "exceeded max offset" and "overwrites values in the frame". That is what
/// `.github/workflows/programs-ci.yml` now does.
///
/// Boxing moves each deserialized account to the heap and leaves a pointer
/// in the frame. All six are boxed rather than the one or two needed to
/// claw back eight bytes, because landing at 4094 would mean the next
/// account added here — or a field added to `StakingConfig`, which is a
/// governance parameter array and will grow — silently reintroduces this.
/// `escrow::ExecuteDisputeOutcome` carries the same instruction and for the
/// same reason.
#[derive(Accounts)]
pub struct RecoverStakeShortfall<'info> {
    /// Rent for the receipt on its first recovery. Pays and signs nothing
    /// else — this account confers no authority whatever, and is not the
    /// merchant.
    #[account(mut)]
    pub payer: Signer<'info>,

    #[account(mint::token_program = token_program)]
    pub mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(
        seeds = [STAKING_CONFIG_SEED],
        bump = staking_config.bump,
        constraint = staking_config.mint == mint.key() @ ErrorCode::WrongMint,
    )]
    pub staking_config: Box<Account<'info, StakingConfig>>,

    /// The merchant's **Merchant-role** stake, and only that. The seeds
    /// pin owner and role together, so an arbitrator's or a node
    /// operator's bond cannot be drawn on to settle a merchant's dispute
    /// debt — those are separate positions backing separate promises.
    #[account(
        mut,
        seeds = [STAKE_ACCOUNT_SEED, stake_account.owner.as_ref(), &[Role::Merchant as u8]],
        bump = stake_account.bump,
        constraint = stake_account.role == Role::Merchant @ ErrorCode::NotAStakeAccount,
    )]
    pub stake_account: Box<Account<'info, StakeAccount>>,

    #[account(mut, seeds = [STAKE_VAULT_SEED], bump = staking_config.stake_vault_bump)]
    pub stake_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    /// CHECK: pinned to `openfiat-escrow`'s claim PDA for *this* stake
    /// account's owner by the seeds below, then decoded and re-checked by
    /// [`escrow_claim::read_stake_recovery_claim`]. The seeds are what
    /// remove the caller's choice of account; the decoder only classifies
    /// the one they were forced to bring.
    #[account(
        seeds = [
            escrow_claim::STAKE_RECOVERY_CLAIM_SEED,
            stake_account.owner.as_ref(),
            mint.key().as_ref(),
        ],
        bump,
        seeds::program = escrow_claim::ESCROW_PROGRAM_ID,
    )]
    pub recovery_claim: UncheckedAccount<'info>,

    #[account(
        init_if_needed,
        payer = payer,
        space = 8 + StakeRecoveryReceipt::INIT_SPACE,
        seeds = [STAKE_RECOVERY_RECEIPT_SEED, stake_account.owner.as_ref()],
        bump,
    )]
    pub recovery_receipt: Box<Account<'info, StakeRecoveryReceipt>>,

    /// The merchant's own OPEN liquidity token vault in `openfiat-escrow`,
    /// and the only place recovered stake can go.
    ///
    /// Derived under `escrow`'s program id from the same owner and mint,
    /// so the destination is not a parameter in any meaningful sense: a
    /// caller cannot route the recovery to themselves, to a treasury, or
    /// to another merchant. That matters more here than almost anywhere
    /// else in this program, because this is the one instruction that
    /// moves someone's stake without their signature.
    #[account(
        mut,
        seeds = [
            escrow_claim::LIQUIDITY_VAULT_TOKENS_SEED,
            stake_account.owner.as_ref(),
            mint.key().as_ref(),
        ],
        bump,
        seeds::program = escrow_claim::ESCROW_PROGRAM_ID,
    )]
    pub merchant_open_token_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

pub fn handle_recover_stake_shortfall(ctx: Context<RecoverStakeShortfall>) -> Result<()> {
    let merchant = ctx.accounts.stake_account.owner;
    let mint_key = ctx.accounts.mint.key();

    let claim = escrow_claim::read_stake_recovery_claim(
        &ctx.accounts.recovery_claim.to_account_info(),
        &merchant,
        &mint_key,
    )?;
    let owed_total = claim.map(|claim| claim.owed_total).unwrap_or(0);
    let outstanding =
        escrow_claim::outstanding(owed_total, ctx.accounts.recovery_receipt.recovered_total);
    require!(outstanding > 0, ErrorCode::NothingToRecover);

    // Unbonding first — see this instruction's own doc. Both tranches sit
    // in the same vault, so this is purely a question of which counter is
    // decremented, and taking the one that is already on its way out keeps
    // the merchant's remaining *active* stake — the balance their role
    // eligibility is judged on — intact for as long as possible.
    let from_unbonding = outstanding.min(ctx.accounts.stake_account.unbonding_amount);
    let from_active = outstanding
        .saturating_sub(from_unbonding)
        .min(ctx.accounts.stake_account.amount);
    let amount = from_unbonding
        .checked_add(from_active)
        .ok_or(ErrorCode::Overflow)?;
    // A merchant with a debt and no stake left. The claim stands, the
    // withdrawal gate stays shut on an empty balance, and the caller is
    // told plainly rather than being handed a successful no-op.
    require!(amount > 0, ErrorCode::NoStakeToRecoverFrom);

    let bump = ctx.accounts.staking_config.bump;
    let signer_seeds: &[&[u8]] = &[STAKING_CONFIG_SEED, &[bump]];
    transfer_checked(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: ctx.accounts.stake_vault.to_account_info(),
                mint: ctx.accounts.mint.to_account_info(),
                to: ctx.accounts.merchant_open_token_vault.to_account_info(),
                authority: ctx.accounts.staking_config.to_account_info(),
            },
            &[signer_seeds],
        ),
        amount,
        ctx.accounts.mint.decimals,
    )?;

    let min_stake = ctx.accounts.staking_config.min_stake_for(Role::Merchant);
    let stake_account = &mut ctx.accounts.stake_account;
    stake_account.unbonding_amount = stake_account
        .unbonding_amount
        .checked_sub(from_unbonding)
        .ok_or(ErrorCode::Overflow)?;
    stake_account.amount = stake_account
        .amount
        .checked_sub(from_active)
        .ok_or(ErrorCode::Overflow)?;
    // Same invariant `slash` maintains: a zero balance means a stopped age
    // clock, so an emptied position cannot later present an age it did not
    // hold capital through. A partial recovery leaves the clock running —
    // paying a debt is not misconduct, and there is no argument at all for
    // costing an honest merchant their accrued age over it.
    if stake_account.amount == 0 {
        stake_account.first_staked_at = 0;
    }

    let receipt = &mut ctx.accounts.recovery_receipt;
    receipt.merchant = merchant;
    receipt.bump = ctx.bumps.recovery_receipt;
    receipt.recovered_total = receipt
        .recovered_total
        .checked_add(amount)
        .ok_or(ErrorCode::Overflow)?;
    receipt.recovery_count = receipt
        .recovery_count
        .checked_add(1)
        .ok_or(ErrorCode::Overflow)?;

    emit!(StakeShortfallRecovered {
        stake_account: ctx.accounts.stake_account.key(),
        merchant,
        claim: ctx.accounts.recovery_claim.key(),
        amount,
        from_unbonding,
        from_active,
        owed_total,
        recovered_total: ctx.accounts.recovery_receipt.recovered_total,
        outstanding: escrow_claim::outstanding(
            owed_total,
            ctx.accounts.recovery_receipt.recovered_total,
        ),
        remaining_stake: ctx.accounts.stake_account.amount,
        remaining_unbonding: ctx.accounts.stake_account.unbonding_amount,
        eligible_after: ctx.accounts.stake_account.amount >= min_stake,
        destination: ctx.accounts.merchant_open_token_vault.key(),
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}
