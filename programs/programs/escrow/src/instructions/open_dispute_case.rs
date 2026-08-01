use anchor_lang::prelude::*;
// Brings `SlotHashes::id()` into scope for the `address` constraint, so the
// sysvar address is derived from the type rather than pasted as a literal.
use anchor_lang::solana_program::sysvar::SysvarId;
use anchor_spl::token_interface::{
    transfer_checked, Mint, TokenAccount, TokenInterface, TransferChecked,
};
use openfiat_programs_shared::VaultState;

use crate::{
    constants::*,
    error::ErrorCode,
    events::{ArbitrationDepositTaken, StakeRecoveryClaimRecorded},
    state::*,
};

/// Opens a dispute case and freezes the trade escrow in one atomic step
/// (Phase 4b, plan decision #2) — replaces Phase 4's standalone
/// `freeze_on_dispute`/`dispute_authority` design, which trusted an
/// external signer with no on-chain proof of a real dispute. Callable by
/// either party to the trade, matching OFS-2400 §5 ("a dispute MAY be
/// initiated when... buyer disagrees... merchant reports...").
///
/// # The arbitration deposit
///
/// Opening a case takes `FeeConfig.dispute_filing_fee` in OPEN from the
/// **merchant's** liquidity vault, whoever opened it. The asymmetry is
/// deliberate: a buyer is frequently a one-time participant, and any cost
/// to raising a dispute falls hardest on exactly the party the dispute
/// system exists to protect. The merchant runs an ongoing business off an
/// ongoing vault, so they are the side that can carry it.
///
/// The deposit is held in the arbitration pool until
/// `execute_dispute_outcome` settles it — forfeited to the arbitrators
/// who decided the case if the merchant is found at fault, returned to
/// their vault otherwise.
///
/// **An underfunded merchant does not block the dispute.** If the vault
/// cannot cover the deposit, the case opens with whatever it could cover
/// (down to nothing) and the shortfall is recorded in
/// [`ArbitrationDepositTaken`]. Requiring the full deposit would hand a
/// merchant a trivial way to make themselves undisputable — keep the OPEN
/// vault empty and no buyer can ever open a case against you. Losing an
/// arbitrator reward on one case is a far smaller harm than a buyer with
/// no recourse, so the case wins.
///
/// `[PROPOSED — NEEDS SIGN-OFF]`: that partial/zero collection is
/// preferable to refusing the case. The natural complement is requiring a
/// funded OPEN vault before a merchant may list at all — which
/// `charge_ad_listing_fee` already pushes them toward, since listing draws
/// on the same vault.
///
/// # The shortfall is now a debt, not just a log line
///
/// OFS-4100 §9.3 makes the merchant's stake the backstop for exactly this
/// case. Until now the shortfall was computed, emitted, and discarded, so
/// nothing downstream could act on it: an under-funded merchant kept a
/// full stake and the arbitrators who decided their case were paid out of
/// a deposit that was never collected.
///
/// So opening a case now also records the shortfall on the merchant's
/// [`StakeRecoveryClaim`], the account `openfiat-staking` reads before it
/// will let stake leave. Recording it **here, at open** — rather than at
/// resolution, which is where §9.3's table puts it — is the one decision
/// in this design that is not a restatement of the specification, and it
/// is what makes the rest of it enforceable:
///
/// A merchant's unbonding period is 24 hours (OFS-4100 §4). A dispute's
/// commit and reveal windows can each run to a week. A debt that comes
/// into existence when the case *closes* is therefore a debt against stake
/// the merchant has had days to withdraw in full, entirely legally, while
/// the case was still running. Recovery at close is not a weaker rule than
/// recovery at open; against a merchant who is paying attention it is no
/// rule at all.
///
/// Opening the debt at open costs the merchant nothing they do not already
/// owe. The deposit is refundable — if the case does not go against them
/// it returns to their vault, exactly as a deposit funded in cash would
/// have. `[PROPOSED — NEEDS SIGN-OFF]`: the timing, and the consequence
/// that a merchant who wins gets their deposit back as vault liquidity
/// rather than as restored stake.
#[derive(Accounts)]
pub struct OpenDisputeCase<'info> {
    #[account(constraint = signer.key() == trade_escrow.buyer || signer.key() == trade_escrow.seller @ ErrorCode::NotAPartyToThisTrade)]
    pub signer: Signer<'info>,

    #[account(mut)]
    pub payer: Signer<'info>,

    #[account(
        mut,
        seeds = [TRADE_ESCROW_SEED, &trade_escrow.reservation_id.to_le_bytes()],
        bump = trade_escrow.bump,
        constraint = trade_escrow.state == VaultState::AwaitingFiatSettlement @ ErrorCode::InvalidVaultState,
    )]
    pub trade_escrow: Account<'info, TradeEscrowVault>,

    #[account(
        init,
        payer = payer,
        space = 8 + DisputeCase::INIT_SPACE,
        seeds = [DISPUTE_CASE_SEED, &trade_escrow.reservation_id.to_le_bytes()],
        bump
    )]
    pub dispute_case: Box<Account<'info, DisputeCase>>,

    #[account(seeds = [FEE_CONFIG_SEED], bump = fee_config.bump)]
    pub fee_config: Box<Account<'info, FeeConfig>>,

    /// The OPEN mint the deposit is denominated in (OFS-4100 §6).
    #[account(mint::token_program = token_program)]
    pub deposit_mint: Box<InterfaceAccount<'info, Mint>>,

    /// The merchant's OPEN liquidity vault. Seeds pin it to
    /// `trade_escrow.seller`, so the deposit cannot be sourced from
    /// anyone else's vault — including the opener's, when the opener is
    /// the buyer.
    #[account(
        mut,
        seeds = [LIQUIDITY_VAULT_SEED, trade_escrow.seller.as_ref(), deposit_mint.key().as_ref()],
        bump = merchant_open_vault.bump,
        constraint = merchant_open_vault.mint == deposit_mint.key(),
    )]
    pub merchant_open_vault: Box<Account<'info, LiquidityVault>>,

    #[account(
        mut,
        seeds = [LIQUIDITY_VAULT_TOKENS_SEED, trade_escrow.seller.as_ref(), deposit_mint.key().as_ref()],
        bump = merchant_open_vault.token_vault_bump,
    )]
    pub merchant_open_token_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        mut,
        seeds = [ARBITRATION_POOL_SEED],
        bump,
        constraint = arbitration_pool.mint == deposit_mint.key(),
    )]
    pub arbitration_pool: Box<InterfaceAccount<'info, TokenAccount>>,

    /// This merchant's running arbitration-deposit debt (OFS-4100 §9.3).
    ///
    /// `init_if_needed` rather than `init` because one merchant accrues
    /// debt across many cases, and rather than a conditional init because
    /// Anchor fixes the account list per instruction, not per branch — a
    /// case that opens fully funded still passes the account and simply
    /// leaves the counters alone. The rent is the opener's, once per
    /// merchant, and it buys `openfiat-staking` an address it can derive
    /// from a stake account's owner without asking anyone.
    #[account(
        init_if_needed,
        payer = payer,
        space = 8 + StakeRecoveryClaim::INIT_SPACE,
        seeds = [STAKE_RECOVERY_CLAIM_SEED, trade_escrow.seller.as_ref(), deposit_mint.key().as_ref()],
        bump
    )]
    pub stake_recovery_claim: Box<Account<'info, StakeRecoveryClaim>>,

    pub token_program: Interface<'info, TokenInterface>,

    pub system_program: Program<'info, System>,

    /// CHECK: pinned to the real sysvar by `address`, then read as raw
    /// bytes — it is far too large to deserialize inside a program, and
    /// `Sysvar::get()` is unsupported for it for that reason. Seeds this
    /// case's arbitrator draw; see `shared_logic::latch_case_seed`, which
    /// is also explicit about the grinding this does *not* prevent.
    #[account(address = SlotHashes::id())]
    pub slot_hashes: UncheckedAccount<'info>,
}

pub fn handle_open_dispute_case(
    ctx: Context<OpenDisputeCase>,
    commit_window_secs: i64,
    reveal_window_secs: i64,
) -> Result<()> {
    // The opener is a party to the trade and picks both windows, so
    // neither may be arbitrary. Too short locks honest arbitrators out of
    // a case the opener is already prepared for; too long parks the other
    // side's funds in `Frozen` indefinitely at no cost.
    require!(
        (MIN_DISPUTE_WINDOW_SECS..=MAX_DISPUTE_WINDOW_SECS).contains(&commit_window_secs),
        ErrorCode::DisputeWindowOutOfRange
    );
    require!(
        (MIN_DISPUTE_WINDOW_SECS..=MAX_DISPUTE_WINDOW_SECS).contains(&reveal_window_secs),
        ErrorCode::DisputeWindowOutOfRange
    );

    let now = Clock::get()?.unix_timestamp;
    let commit_deadline = now
        .checked_add(commit_window_secs)
        .ok_or(ErrorCode::Overflow)?;
    let reveal_deadline = commit_deadline
        .checked_add(reveal_window_secs)
        .ok_or(ErrorCode::Overflow)?;

    // Take what the merchant's vault can cover, rather than requiring the
    // full amount — see this instruction's own doc for why an underfunded
    // merchant must not be able to refuse arbitration.
    let configured = ctx.accounts.fee_config.dispute_filing_fee;
    let available = ctx.accounts.merchant_open_vault.available;
    let deposit = configured.min(available);
    let shortfall = configured.checked_sub(deposit).ok_or(ErrorCode::Overflow)?;

    if deposit > 0 {
        let seller_key = ctx.accounts.trade_escrow.seller;
        let mint_key = ctx.accounts.deposit_mint.key();
        let vault_bump = ctx.accounts.merchant_open_vault.bump;
        let signer_seeds: &[&[u8]] = &[
            LIQUIDITY_VAULT_SEED,
            seller_key.as_ref(),
            mint_key.as_ref(),
            &[vault_bump],
        ];
        transfer_checked(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.key(),
                TransferChecked {
                    from: ctx.accounts.merchant_open_token_vault.to_account_info(),
                    mint: ctx.accounts.deposit_mint.to_account_info(),
                    to: ctx.accounts.arbitration_pool.to_account_info(),
                    authority: ctx.accounts.merchant_open_vault.to_account_info(),
                },
                &[signer_seeds],
            ),
            deposit,
            ctx.accounts.deposit_mint.decimals,
        )?;

        let vault = &mut ctx.accounts.merchant_open_vault;
        vault.available = vault
            .available
            .checked_sub(deposit)
            .ok_or(ErrorCode::Overflow)?;
        vault.total = vault
            .total
            .checked_sub(deposit)
            .ok_or(ErrorCode::Overflow)?;
    }

    emit!(ArbitrationDepositTaken {
        reservation_id: ctx.accounts.trade_escrow.reservation_id,
        merchant: ctx.accounts.trade_escrow.seller,
        opened_by: ctx.accounts.signer.key(),
        deposit_vault: ctx.accounts.merchant_open_vault.key(),
        mint: ctx.accounts.deposit_mint.key(),
        amount: deposit,
        shortfall,
        timestamp: now,
    });

    // Written unconditionally, incremented only when there is a shortfall.
    // `init_if_needed` leaves an existing account's bytes intact, so these
    // three are idempotent re-statements on every case after the first
    // rather than a reset — and on the first they are what makes the
    // account self-describing to a reader that only knows its address.
    let claim_key = ctx.accounts.stake_recovery_claim.key();
    let claim = &mut ctx.accounts.stake_recovery_claim;
    claim.merchant = ctx.accounts.trade_escrow.seller;
    claim.mint = ctx.accounts.deposit_mint.key();
    claim.bump = ctx.bumps.stake_recovery_claim;
    if shortfall > 0 {
        claim.owed_total = claim
            .owed_total
            .checked_add(shortfall)
            .ok_or(ErrorCode::Overflow)?;
        claim.case_count = claim.case_count.checked_add(1).ok_or(ErrorCode::Overflow)?;
        emit!(StakeRecoveryClaimRecorded {
            reservation_id: ctx.accounts.trade_escrow.reservation_id,
            merchant: claim.merchant,
            claim: claim_key,
            mint: claim.mint,
            shortfall,
            owed_total: claim.owed_total,
            credited_total: claim.credited_total,
            case_count: claim.case_count,
            timestamp: now,
        });
    }

    let dispute_case = &mut ctx.accounts.dispute_case;
    dispute_case.deposit_vault = ctx.accounts.merchant_open_vault.key();
    dispute_case.deposit_mint = ctx.accounts.deposit_mint.key();
    dispute_case.deposit = deposit;
    dispute_case.deposit_shortfall = shortfall;
    dispute_case.outcome = None;
    dispute_case.winning_weight = 0;
    dispute_case.reward_pool = 0;
    dispute_case.reward_remaining = 0;
    dispute_case.deposit_settled = false;
    dispute_case.reservation_id = ctx.accounts.trade_escrow.reservation_id;
    dispute_case.trade_escrow = ctx.accounts.trade_escrow.key();
    dispute_case.opened_at = now;
    dispute_case.round_opened_at = now;
    dispute_case.commit_deadline = commit_deadline;
    dispute_case.reveal_deadline = reveal_deadline;
    dispute_case.resolved = false;
    dispute_case.round = 0;
    dispute_case.commit_window_secs = commit_window_secs;
    dispute_case.reveal_window_secs = reveal_window_secs;
    dispute_case.case_seed = crate::instructions::shared_logic::latch_case_seed(
        &ctx.accounts.slot_hashes.to_account_info(),
        ctx.accounts.trade_escrow.reservation_id,
        &ctx.accounts.trade_escrow.key(),
    )?;
    dispute_case.arbitrators = Vec::new();
    dispute_case.commitments = Vec::new();
    dispute_case.revealed_outcomes = Vec::new();
    dispute_case.weights = Vec::new();
    dispute_case.reward_claimed = Vec::new();
    dispute_case.barred = Vec::new();
    dispute_case.bump = ctx.bumps.dispute_case;

    ctx.accounts.trade_escrow.state = VaultState::Frozen;
    Ok(())
}
