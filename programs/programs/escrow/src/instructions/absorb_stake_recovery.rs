use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, TokenAccount};

use crate::{constants::*, error::ErrorCode, events::StakeRecoveryAbsorbed, state::*};

/// Credits a merchant's OPEN liquidity vault with stake that
/// `openfiat-staking` has already moved into it (OFS-4100 §9.3).
///
/// # Permissionless, and self-verifying
///
/// Anyone may call this; nobody is trusted by it. The amount is not a
/// parameter — there is no parameter. It is the difference between two
/// counters this instruction reads: `staking::StakeRecoveryReceipt`'s
/// `recovered_total`, which only `openfiat-staking` writes, and this
/// claim's own `credited_total`, which only this program writes. A caller
/// chooses which merchant to run it for and nothing else, which is the
/// same shape `execute_dispute_outcome` has: the caller supplies the
/// occasion, the accounts supply the facts.
///
/// # Why the tokens arrive before the accounting does
///
/// `openfiat-staking` transferred them out of the stake vault in an
/// earlier transaction. It could not credit `LiquidityVault`'s counters
/// while doing so — that account belongs to this program, and OFS-4200 §1
/// forbids the CPI that would let one program write the other's state (a
/// `staking -> escrow` dependency would also close a Cargo cycle, since
/// this program already depends on `staking`).
///
/// So between the two transactions the vault holds a balance its own
/// accounting does not know about. That is not a hazard — every path that
/// spends from the vault spends against `available`, so an uncredited
/// balance is simply unusable — but it is a discrepancy, and this
/// instruction is what closes it. The token balance is checked rather than
/// assumed: if the receipt claims a recovery the vault cannot account for,
/// this refuses instead of inventing liquidity.
///
/// # Who is motivated to call it
///
/// The merchant, most directly — until it runs, tokens taken out of their
/// stake are not spendable liquidity. But it is deliberately not gated on
/// their signature: the arbitrators on an under-funded case are waiting on
/// the same credit reaching `top_up_arbitration_deposit`, and a merchant
/// who has just been made to pay is the last party who should be able to
/// stall it.
#[derive(Accounts)]
pub struct AbsorbStakeRecovery<'info> {
    /// The OPEN mint the claim and the merchant's vault are both
    /// denominated in.
    pub mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(
        mut,
        seeds = [STAKE_RECOVERY_CLAIM_SEED, claim.merchant.as_ref(), mint.key().as_ref()],
        bump = claim.bump,
        constraint = claim.mint == mint.key(),
    )]
    pub claim: Box<Account<'info, StakeRecoveryClaim>>,

    /// `openfiat-staking`'s record of what it has taken out of this
    /// merchant's stake.
    ///
    /// Read as a real typed account rather than raw bytes because this
    /// direction of the dependency is the legal one — `escrow` already
    /// depends on `staking` to weigh dispute votes, so the type is simply
    /// in scope. The seeds are pinned to `staking::ID` so the account
    /// cannot be a look-alike written by anything else.
    #[account(
        seeds = [staking::STAKE_RECOVERY_RECEIPT_SEED, claim.merchant.as_ref()],
        seeds::program = staking::ID,
        bump = recovery_receipt.bump,
        constraint = recovery_receipt.merchant == claim.merchant,
    )]
    pub recovery_receipt: Box<Account<'info, staking::StakeRecoveryReceipt>>,

    #[account(
        mut,
        seeds = [LIQUIDITY_VAULT_SEED, claim.merchant.as_ref(), mint.key().as_ref()],
        bump = merchant_open_vault.bump,
        constraint = merchant_open_vault.mint == mint.key(),
    )]
    pub merchant_open_vault: Box<Account<'info, LiquidityVault>>,

    /// Read-only, and read for exactly one reason: to confirm the tokens
    /// the receipt says were recovered are actually sitting here before
    /// the counters are told they are.
    #[account(
        seeds = [LIQUIDITY_VAULT_TOKENS_SEED, claim.merchant.as_ref(), mint.key().as_ref()],
        bump = merchant_open_vault.token_vault_bump,
    )]
    pub merchant_open_token_vault: Box<InterfaceAccount<'info, TokenAccount>>,
}

pub fn handle_absorb_stake_recovery(ctx: Context<AbsorbStakeRecovery>) -> Result<()> {
    let recovered_total = ctx.accounts.recovery_receipt.recovered_total;
    let amount = ctx.accounts.claim.absorbable(recovered_total);
    require!(amount > 0, ErrorCode::NothingToAbsorb);

    // The vault's tracked balance plus what is about to be credited must
    // already be present in the token account. Defence in depth rather
    // than paranoia: the receipt is written by another program, and the
    // failure it guards against — crediting liquidity that no transfer
    // ever delivered — would let a vault spend tokens it does not hold and
    // fail at CPI time in some later, unrelated instruction.
    //
    // It is also what makes the receipt's single counter safe. The receipt
    // is keyed by merchant alone while a claim is keyed by merchant *and*
    // mint, so a merchant holding claims in two mints would have one
    // `recovered_total` and two vaults it could be pointed at. Recovery
    // only ever pays into the vault of `staking_config.mint`, so the other
    // vault never receives the tokens — and this check is what turns that
    // into a refusal rather than a credit of liquidity that is sitting
    // somewhere else.
    let backed = ctx
        .accounts
        .merchant_open_vault
        .total
        .checked_add(amount)
        .ok_or(ErrorCode::Overflow)?;
    require!(
        ctx.accounts.merchant_open_token_vault.amount >= backed,
        ErrorCode::RecoveredTokensMissing
    );

    let vault = &mut ctx.accounts.merchant_open_vault;
    vault.total = backed;
    vault.available = vault
        .available
        .checked_add(amount)
        .ok_or(ErrorCode::Overflow)?;

    let claim = &mut ctx.accounts.claim;
    claim.credited_total = claim
        .credited_total
        .checked_add(amount)
        .ok_or(ErrorCode::Overflow)?;

    emit!(StakeRecoveryAbsorbed {
        merchant: claim.merchant,
        claim: claim.key(),
        mint: claim.mint,
        amount,
        owed_total: claim.owed_total,
        credited_total: claim.credited_total,
        // What the merchant's stake has still not covered. Reported on
        // every absorb, including the ones that fully settle the debt, so
        // a reader never has to subtract two events to learn they are
        // still owed something.
        outstanding: claim.owed_total.saturating_sub(recovered_total),
        vault_available: ctx.accounts.merchant_open_vault.available,
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}
