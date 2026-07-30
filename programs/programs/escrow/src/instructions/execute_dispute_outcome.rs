use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    transfer_checked, Mint, Token2022, TokenAccount, TransferChecked,
};
use openfiat_programs_shared::{DisputeOutcome, VaultState};

use crate::events::{DisputeResolved, EscrowReleased};
use crate::instructions::shared_logic::{
    release_trade_escrow_funds, split_trade_escrow_evenly, unwind_funded_trade_escrow,
};
use crate::{constants::*, error::ErrorCode, state::*};

/// Permissionless, callable once the reveal window has closed — tallies
/// `dispute_case`'s own on-chain-recorded, stake-weighted votes itself
/// rather than trusting a caller-supplied outcome (Phase 4b, plan
/// decision #2).
///
/// `BuyerWins` releases funds identically to `release_escrow`.
/// `MerchantWins` and `InvalidDispute` return them to the liquidity
/// vault. `MutualSettlement` splits evenly — previously it fell in with
/// the merchant-wins arm, which handed the seller everything under a
/// verdict that by name means neither side wholly won.
///
/// A round that decides nothing pays nobody and re-opens the case; see
/// `handle_undecided_round`.
#[derive(Accounts)]
pub struct ExecuteDisputeOutcome<'info> {
    pub mint: InterfaceAccount<'info, Mint>,

    #[account(
        mut,
        seeds = [DISPUTE_CASE_SEED, &dispute_case.reservation_id.to_le_bytes()],
        bump = dispute_case.bump,
        constraint = !dispute_case.resolved @ ErrorCode::DisputeAlreadyResolved,
    )]
    pub dispute_case: Box<Account<'info, DisputeCase>>,

    #[account(
        mut,
        seeds = [TRADE_ESCROW_SEED, &trade_escrow.reservation_id.to_le_bytes()],
        bump = trade_escrow.bump,
        constraint = trade_escrow.key() == dispute_case.trade_escrow,
        constraint = trade_escrow.mint == mint.key(),
        constraint = trade_escrow.state == VaultState::Frozen @ ErrorCode::InvalidVaultState,
    )]
    pub trade_escrow: Box<Account<'info, TradeEscrowVault>>,

    #[account(
        mut,
        seeds = [TRADE_ESCROW_TOKENS_SEED, &trade_escrow.reservation_id.to_le_bytes()],
        bump = trade_escrow.token_vault_bump,
    )]
    pub trade_escrow_token_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        mut,
        seeds = [LIQUIDITY_VAULT_SEED, trade_escrow.seller.as_ref(), mint.key().as_ref()],
        bump = liquidity_vault.bump,
        constraint = liquidity_vault.mint == mint.key(),
    )]
    pub liquidity_vault: Box<Account<'info, LiquidityVault>>,

    #[account(
        mut,
        seeds = [LIQUIDITY_VAULT_TOKENS_SEED, trade_escrow.seller.as_ref(), mint.key().as_ref()],
        bump = liquidity_vault.token_vault_bump,
    )]
    pub liquidity_token_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut, constraint = buyer_token_account.owner == trade_escrow.buyer, constraint = buyer_token_account.mint == mint.key())]
    pub buyer_token_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        seeds = [FEE_CONFIG_SEED],
        bump = fee_config.bump,
        constraint = fee_config.dev_treasury == dev_treasury.key(),
        constraint = fee_config.ecosystem_treasury == ecosystem_treasury.key(),
        constraint = fee_config.infra_treasury == infra_treasury.key(),
        constraint = fee_config.emergency_reserve == emergency_reserve.key(),
    )]
    pub fee_config: Box<Account<'info, FeeConfig>>,

    #[account(mut)]
    pub dev_treasury: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub ecosystem_treasury: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub infra_treasury: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub emergency_reserve: Box<InterfaceAccount<'info, TokenAccount>>,

    /// The OPEN mint the arbitration deposit is held in — distinct from
    /// `mint`, which is the settlement stablecoin.
    #[account(constraint = deposit_mint.key() == dispute_case.deposit_mint)]
    pub deposit_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(
        mut,
        seeds = [ARBITRATION_POOL_SEED],
        bump,
        constraint = arbitration_pool.mint == deposit_mint.key(),
    )]
    pub arbitration_pool: Box<InterfaceAccount<'info, TokenAccount>>,

    /// The merchant's OPEN vault the deposit came from, and where it
    /// returns when the merchant is not found at fault.
    ///
    /// Must not be the same account as `liquidity_vault`. It cannot be
    /// when the trade settles in a stablecoin and the deposit is in OPEN,
    /// which is the intended configuration — but a trade whose settlement
    /// mint *is* OPEN would make both seeds resolve to one vault, and
    /// Anchor would then deserialize it into two independent structs and
    /// write only one of them back, silently losing the other's balance
    /// updates. Rejecting it is better than corrupting a vault; supporting
    /// OPEN-settled trades through the dispute path would mean merging
    /// both updates into a single account handle.
    #[account(
        mut,
        constraint = merchant_open_vault.key() == dispute_case.deposit_vault,
        constraint = merchant_open_vault.key() != liquidity_vault.key() @ ErrorCode::DepositVaultAliasesSettlementVault,
    )]
    pub merchant_open_vault: Box<Account<'info, LiquidityVault>>,

    #[account(
        mut,
        seeds = [LIQUIDITY_VAULT_TOKENS_SEED, merchant_open_vault.merchant.as_ref(), deposit_mint.key().as_ref()],
        bump = merchant_open_vault.token_vault_bump,
    )]
    pub merchant_open_token_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    pub token_program: Program<'info, Token2022>,
}

/// Where an arbitration deposit goes once a case ends.
///
/// The steward's rule is that a merchant found at fault forfeits it to the
/// arbitration pool. The two cases the rule does not name are marked
/// `[PROPOSED — NEEDS SIGN-OFF]` below and resolved the way the word
/// "deposit" ordinarily implies: it comes back when you are not at fault.
fn settle_deposit(
    ctx: &mut Context<ExecuteDisputeOutcome>,
    outcome: Option<DisputeOutcome>,
) -> Result<(u64, u64)> {
    if ctx.accounts.dispute_case.deposit_settled || ctx.accounts.dispute_case.deposit == 0 {
        return Ok((0, 0));
    }
    let deposit = ctx.accounts.dispute_case.deposit;

    // Forfeited only where the merchant actually lost. `BuyerWins` is the
    // one verdict that means that.
    //
    // `MerchantWins` and `InvalidDispute` return it: the merchant was not
    // at fault. `MutualSettlement` and the undecided terminal split
    // (`None`) also return it — nobody was found at fault there either,
    // and forfeiting on "no consensus was reached" would punish a merchant
    // for an outcome the arbitrators failed to produce.
    // `[PROPOSED — NEEDS SIGN-OFF]` for every case except `BuyerWins`.
    let forfeited = matches!(outcome, Some(DisputeOutcome::BuyerWins));

    if !forfeited {
        let fee_bump = ctx.accounts.fee_config.bump;
        let signer_seeds: &[&[u8]] = &[FEE_CONFIG_SEED, &[fee_bump]];
        transfer_checked(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.key(),
                TransferChecked {
                    from: ctx.accounts.arbitration_pool.to_account_info(),
                    mint: ctx.accounts.deposit_mint.to_account_info(),
                    to: ctx.accounts.merchant_open_token_vault.to_account_info(),
                    authority: ctx.accounts.fee_config.to_account_info(),
                },
                &[signer_seeds],
            ),
            deposit,
            ctx.accounts.deposit_mint.decimals,
        )?;
        let vault = &mut ctx.accounts.merchant_open_vault;
        vault.available = vault
            .available
            .checked_add(deposit)
            .ok_or(ErrorCode::Overflow)?;
        vault.total = vault
            .total
            .checked_add(deposit)
            .ok_or(ErrorCode::Overflow)?;
    }

    let case = &mut ctx.accounts.dispute_case;
    case.deposit_settled = true;
    if forfeited {
        case.reward_pool = deposit;
        case.reward_remaining = deposit;
        Ok((deposit, 0))
    } else {
        Ok((0, deposit))
    }
}

/// Sums each revealed vote's weight per outcome and returns the winner,
/// or `None` when the round reached no decision.
///
/// Zero-weight reveals are skipped outright rather than counted as votes
/// worth nothing. Counting them was load-bearing in the seat-squatting
/// attack `commit_dispute_vote` now gates against: with every weight
/// zero, all four totals tied at zero and the tie resolved to a real
/// outcome. Skipping them is the second line of defence, so the tally is
/// safe even if a zero-stake account ever reaches it again.
///
/// A genuine tie between two funded outcomes also returns `None`. It used
/// to resolve to `InvalidDispute`, which pays the seller — meaning any
/// party able to manufacture indecision was paid for it. Deciding nothing
/// must never be worth more than losing.
fn tally(dispute_case: &DisputeCase) -> Option<DisputeOutcome> {
    let mut totals = [0u128; 4];
    let mut counted = 0usize;
    for (outcome, weight) in dispute_case
        .revealed_outcomes
        .iter()
        .zip(dispute_case.weights.iter())
    {
        if *weight == 0 {
            continue;
        }
        if let Some(outcome) = outcome {
            totals[*outcome as usize] += *weight as u128;
            counted += 1;
        }
    }
    // Fewer than `MIN_ARBITRATORS` counted votes is not a decision, however
    // lopsided the weights are. One arbitrator used to be able to settle a
    // dispute alone simply by being the only one who revealed, and a single
    // large stake could still do it here — so the floor is on the number of
    // participants, deliberately independent of how much weight they bring.
    //
    // `counted` rather than `arbitrators.len()`: it counts the votes the
    // totals above actually include, so zero-weight reveals cannot pad the
    // way to a quorum.
    if counted < MIN_ARBITRATORS {
        return None;
    }

    let max = *totals.iter().max().unwrap();
    let winners: Vec<usize> = totals
        .iter()
        .enumerate()
        .filter(|(_, t)| **t == max)
        .map(|(i, _)| i)
        .collect();
    if winners.len() != 1 {
        return None;
    }
    Some(match winners[0] {
        0 => DisputeOutcome::BuyerWins,
        1 => DisputeOutcome::MerchantWins,
        2 => DisputeOutcome::MutualSettlement,
        _ => DisputeOutcome::InvalidDispute,
    })
}

pub fn handle_execute_dispute_outcome(mut ctx: Context<ExecuteDisputeOutcome>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    require!(
        now >= ctx.accounts.dispute_case.reveal_deadline,
        ErrorCode::RevealWindowStillOpen
    );

    let Some(outcome) = tally(&ctx.accounts.dispute_case) else {
        return handle_undecided_round(ctx, now);
    };

    // Total weight behind the winning outcome — the denominator each
    // winning arbitrator's pro-rata claim is computed against.
    let winning_weight: u64 = ctx
        .accounts
        .dispute_case
        .revealed_outcomes
        .iter()
        .zip(ctx.accounts.dispute_case.weights.iter())
        .filter(|(o, _)| **o == Some(outcome))
        .map(|(_, w)| *w)
        .sum();

    let (reward_pool, deposit_refunded) = settle_deposit(&mut ctx, Some(outcome))?;

    match outcome {
        DisputeOutcome::BuyerWins => {
            let amount = ctx.accounts.trade_escrow.amount;
            let (buyer_amount, fee_shares) = release_trade_escrow_funds(
                &mut ctx.accounts.trade_escrow,
                &ctx.accounts.trade_escrow_token_vault,
                &ctx.accounts.buyer_token_account,
                &mut ctx.accounts.liquidity_vault,
                &ctx.accounts.fee_config,
                &ctx.accounts.dev_treasury,
                &ctx.accounts.ecosystem_treasury,
                &ctx.accounts.infra_treasury,
                &ctx.accounts.emergency_reserve,
                &ctx.accounts.mint,
                &ctx.accounts.token_program,
            )?;
            emit!(EscrowReleased {
                reservation_id: ctx.accounts.trade_escrow.reservation_id,
                buyer: ctx.accounts.trade_escrow.buyer,
                seller: ctx.accounts.trade_escrow.seller,
                mint: ctx.accounts.mint.key(),
                amount,
                buyer_amount,
                fee: amount
                    .checked_sub(buyer_amount)
                    .ok_or(ErrorCode::Overflow)?,
                dev_treasury_amount: fee_shares[0],
                ecosystem_treasury_amount: fee_shares[1],
                infra_treasury_amount: fee_shares[2],
                emergency_reserve_amount: fee_shares[3],
                via_dispute: true,
                timestamp: now,
            });
        }
        // Both are verdicts a stake-weighted majority actively chose:
        // the trade stands and the escrow returns to the seller. This is
        // no longer where an undecided round lands.
        DisputeOutcome::MerchantWins | DisputeOutcome::InvalidDispute => {
            unwind_funded_trade_escrow(
                &ctx.accounts.trade_escrow,
                &ctx.accounts.trade_escrow_token_vault,
                &mut ctx.accounts.liquidity_vault,
                &ctx.accounts.liquidity_token_vault,
                &ctx.accounts.mint,
                &ctx.accounts.token_program,
            )?;
            ctx.accounts.trade_escrow.state = VaultState::Cancelled;
        }
        DisputeOutcome::MutualSettlement => {
            split_trade_escrow_evenly(
                &mut ctx.accounts.trade_escrow,
                &ctx.accounts.trade_escrow_token_vault,
                &ctx.accounts.buyer_token_account,
                &mut ctx.accounts.liquidity_vault,
                &ctx.accounts.liquidity_token_vault,
                &ctx.accounts.mint,
                &ctx.accounts.token_program,
            )?;
        }
    }

    let case = &mut ctx.accounts.dispute_case;
    case.resolved = true;
    case.outcome = Some(outcome);
    case.winning_weight = winning_weight;
    // One flag per seat, so `claim_arbitration_reward` can mark seats off
    // individually.
    case.reward_claimed = vec![false; case.arbitrators.len()];

    emit!(DisputeResolved {
        reservation_id: case.reservation_id,
        buyer: ctx.accounts.trade_escrow.buyer,
        seller: ctx.accounts.trade_escrow.seller,
        outcome: Some(outcome),
        round: case.round,
        winning_weight,
        arbitrator_count: case.arbitrators.len() as u8,
        reward_pool,
        deposit_refunded,
        timestamp: now,
    });
    Ok(())
}

/// A round that decided nothing — no qualifying reveal, or a genuine tie.
///
/// Paying either party here is what made the seat-squatting attack worth
/// running, so this does not. Instead the case re-opens for another round
/// with the windows it was created with, leaving the escrow frozen and
/// the funds untouched. Honest arbitrators who missed the first round get
/// another chance; an attacker who filled the seats has to fund and lock
/// the minimum stake again, every round, to keep achieving nothing.
///
/// The retry is bounded, because "re-open forever" is the same permanent
/// freeze by another name. At the limit the escrow splits evenly, so
/// neither side profits from driving a case into the ground — both lose
/// half. See [`MAX_DISPUTE_ROUNDS`] and `split_trade_escrow_evenly`; both
/// the bound and the terminal policy are `[PROPOSED — NEEDS SIGN-OFF]`.
fn handle_undecided_round(mut ctx: Context<ExecuteDisputeOutcome>, now: i64) -> Result<()> {
    let next_round = ctx
        .accounts
        .dispute_case
        .round
        .checked_add(1)
        .ok_or(ErrorCode::Overflow)?;

    if next_round >= MAX_DISPUTE_ROUNDS {
        // Nobody was found at fault, so the deposit goes back to the
        // merchant rather than being forfeited — see `settle_deposit`.
        let (reward_pool, deposit_refunded) = settle_deposit(&mut ctx, None)?;
        split_trade_escrow_evenly(
            &mut ctx.accounts.trade_escrow,
            &ctx.accounts.trade_escrow_token_vault,
            &ctx.accounts.buyer_token_account,
            &mut ctx.accounts.liquidity_vault,
            &ctx.accounts.liquidity_token_vault,
            &ctx.accounts.mint,
            &ctx.accounts.token_program,
        )?;
        let case = &mut ctx.accounts.dispute_case;
        case.resolved = true;
        case.outcome = None;

        emit!(DisputeResolved {
            reservation_id: case.reservation_id,
            buyer: ctx.accounts.trade_escrow.buyer,
            seller: ctx.accounts.trade_escrow.seller,
            outcome: None,
            round: case.round,
            winning_weight: 0,
            arbitrator_count: case.arbitrators.len() as u8,
            reward_pool,
            deposit_refunded,
            timestamp: now,
        });
        return Ok(());
    }

    let dispute_case = &mut ctx.accounts.dispute_case;
    let commit_deadline = now
        .checked_add(dispute_case.commit_window_secs)
        .ok_or(ErrorCode::Overflow)?;
    let reveal_deadline = commit_deadline
        .checked_add(dispute_case.reveal_window_secs)
        .ok_or(ErrorCode::Overflow)?;

    dispute_case.round = next_round;
    dispute_case.commit_deadline = commit_deadline;
    dispute_case.reveal_deadline = reveal_deadline;
    dispute_case.arbitrators = Vec::new();
    dispute_case.commitments = Vec::new();
    dispute_case.revealed_outcomes = Vec::new();
    dispute_case.weights = Vec::new();
    dispute_case.reward_claimed = Vec::new();
    Ok(())
}
