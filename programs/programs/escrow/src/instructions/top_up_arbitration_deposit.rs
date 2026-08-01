use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    transfer_checked, Mint, TokenAccount, TokenInterface, TransferChecked,
};
use openfiat_programs_shared::VaultState;

use crate::{constants::*, error::ErrorCode, events::ArbitrationDepositToppedUp, state::*};

/// Makes an under-funded arbitration deposit good out of the merchant's
/// OPEN liquidity vault (OFS-4100 §9.3).
///
/// # Permissionless, and it decides nothing
///
/// The amount is `min(what this case is still short, what the vault can
/// cover)`. Both numbers are on accounts this instruction reads. There is
/// no parameter to get wrong and no signature that changes the answer —
/// the caller picks the case and that is the whole of their influence.
///
/// # Where the liquidity comes from is deliberately not this instruction's
/// business
///
/// Usually `absorb_stake_recovery`, having just credited the vault with
/// OPEN taken out of the merchant's stake — that is the path OFS-4100
/// §9.3 describes, and the reason this exists. But a merchant who simply
/// calls `deposit_liquidity` funds the same top-up, and so does a case
/// that was short at open because the vault was momentarily drained by an
/// ad-listing fee.
///
/// Keeping the two apart is what makes the stake-recovery relay a relay
/// rather than a special case: staking moves tokens into a vault, this
/// moves vault liquidity into the pool, and neither knows about the
/// other's reason.
///
/// # Only while the case is open
///
/// Once `execute_dispute_outcome` has run, `deposit` has already been
/// forfeited to the arbitrators or returned to the merchant, and
/// `reward_pool`/`reward_remaining` have been divided against it. Growing
/// the deposit after that would either pay a late claimant more than an
/// early one for the same weight, or push tokens into a pool no case will
/// ever pay out. So a resolved case refuses, and the shortfall stays on
/// the record as a debt that outlived the dispute it came from — which is
/// the honest outcome, not a silently absorbed one.
///
/// `[PROPOSED — NEEDS SIGN-OFF]`: refusing rather than back-crediting a
/// resolved case's reward pool.
///
/// # What a completed deposit does and does not buy
///
/// Making the deposit whole does not make it forfeit. `settle_deposit`
/// still returns it whenever the merchant is not found at fault, and the
/// terminal even split after [`MAX_DISPUTE_ROUNDS`] counts as not at
/// fault — nobody was found against there.
///
/// That matters more than it used to. OFS-4100 Annex A shows the even
/// split is structurally reachable on any arbitrator pool below about 17:
/// the barring rule can retire up to 14 wallets across three rounds, so a
/// party with 15 funded wallets (7,500 OPEN at the current floor, locked
/// rather than slashed) can exhaust the pool and force it. A merchant who
/// does that recovers half the escrow **and** their deposit.
///
/// So the honest account of what this relay achieves is narrower than
/// "the merchant always pays": it guarantees the deposit is *collected*
/// and therefore that arbitrators are paid whenever the case decides
/// against the merchant. It does not make an under-funded merchant worse
/// off than a funded one, and it is not a defence against forcing
/// indecision — that is Annex A's problem and needs the pool floor or the
/// stake-age gate, not anything here. What the relay removes is the
/// cheaper attack underneath it: being undisputable by keeping the vault
/// empty.
#[derive(Accounts)]
pub struct TopUpArbitrationDeposit<'info> {
    #[account(
        mut,
        seeds = [DISPUTE_CASE_SEED, &dispute_case.reservation_id.to_le_bytes()],
        bump = dispute_case.bump,
        constraint = !dispute_case.resolved @ ErrorCode::DisputeAlreadyResolved,
    )]
    pub dispute_case: Box<Account<'info, DisputeCase>>,

    /// The frozen trade the case belongs to. Carried only so the merchant
    /// this top-up debits is the seller the case was opened against,
    /// checked against the escrow rather than against a vault the caller
    /// chose.
    #[account(
        seeds = [TRADE_ESCROW_SEED, &dispute_case.reservation_id.to_le_bytes()],
        bump = trade_escrow.bump,
        constraint = trade_escrow.key() == dispute_case.trade_escrow,
        constraint = trade_escrow.state == VaultState::Frozen @ ErrorCode::InvalidVaultState,
    )]
    pub trade_escrow: Box<Account<'info, TradeEscrowVault>>,

    /// The OPEN mint the deposit is denominated in, pinned to the case so
    /// a top-up cannot be routed through a different mint than the one the
    /// original deposit was taken in.
    #[account(
        constraint = mint.key() == dispute_case.deposit_mint,
        mint::token_program = token_program,
    )]
    pub mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(
        mut,
        seeds = [LIQUIDITY_VAULT_SEED, trade_escrow.seller.as_ref(), mint.key().as_ref()],
        bump = merchant_open_vault.bump,
        constraint = merchant_open_vault.key() == dispute_case.deposit_vault,
        constraint = merchant_open_vault.mint == mint.key(),
    )]
    pub merchant_open_vault: Box<Account<'info, LiquidityVault>>,

    #[account(
        mut,
        seeds = [LIQUIDITY_VAULT_TOKENS_SEED, trade_escrow.seller.as_ref(), mint.key().as_ref()],
        bump = merchant_open_vault.token_vault_bump,
    )]
    pub merchant_open_token_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        mut,
        seeds = [ARBITRATION_POOL_SEED],
        bump,
        constraint = arbitration_pool.mint == mint.key(),
    )]
    pub arbitration_pool: Box<InterfaceAccount<'info, TokenAccount>>,

    pub token_program: Interface<'info, TokenInterface>,
}

pub fn handle_top_up_arbitration_deposit(ctx: Context<TopUpArbitrationDeposit>) -> Result<()> {
    let shortfall = ctx.accounts.dispute_case.deposit_shortfall;
    require!(shortfall > 0, ErrorCode::NoDepositShortfall);

    // Take what the vault can cover and no more, exactly as
    // `open_dispute_case` did. A partial top-up is a real outcome, not a
    // failure to handle: the alternative — refusing unless the vault can
    // clear the whole shortfall — would leave a merchant who can pay half
    // paying nothing.
    let amount = shortfall.min(ctx.accounts.merchant_open_vault.available);
    require!(amount > 0, ErrorCode::NoLiquidityForShortfall);

    let merchant = ctx.accounts.trade_escrow.seller;
    let mint_key = ctx.accounts.mint.key();
    let vault_bump = ctx.accounts.merchant_open_vault.bump;
    let signer_seeds: &[&[u8]] = &[
        LIQUIDITY_VAULT_SEED,
        merchant.as_ref(),
        mint_key.as_ref(),
        &[vault_bump],
    ];
    transfer_checked(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: ctx.accounts.merchant_open_token_vault.to_account_info(),
                mint: ctx.accounts.mint.to_account_info(),
                to: ctx.accounts.arbitration_pool.to_account_info(),
                authority: ctx.accounts.merchant_open_vault.to_account_info(),
            },
            &[signer_seeds],
        ),
        amount,
        ctx.accounts.mint.decimals,
    )?;

    let vault = &mut ctx.accounts.merchant_open_vault;
    vault.available = vault
        .available
        .checked_sub(amount)
        .ok_or(ErrorCode::Overflow)?;
    vault.total = vault.total.checked_sub(amount).ok_or(ErrorCode::Overflow)?;
    let vault_available = vault.available;

    let case = &mut ctx.accounts.dispute_case;
    case.deposit = case
        .deposit
        .checked_add(amount)
        .ok_or(ErrorCode::Overflow)?;
    case.deposit_shortfall = case
        .deposit_shortfall
        .checked_sub(amount)
        .ok_or(ErrorCode::Overflow)?;

    emit!(ArbitrationDepositToppedUp {
        reservation_id: case.reservation_id,
        merchant,
        mint: mint_key,
        amount,
        deposit: case.deposit,
        remaining_shortfall: case.deposit_shortfall,
        vault_available,
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}
