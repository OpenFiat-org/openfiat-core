use anchor_lang::prelude::*;
// Brings `SlotHashes::id()` into scope for the `address` constraint, so the
// sysvar address is derived from the type rather than pasted as a literal.
use anchor_lang::solana_program::sysvar::SysvarId;
use anchor_spl::token_interface::{
    transfer_checked, Mint, TokenAccount, TokenInterface, TransferChecked,
};
use openfiat_programs_shared::{DisputeOutcome, VaultState};

use crate::arbitration::{PoolFloor, TerminalSplitReason};
use crate::events::{DisputeResolved, DisputeTerminalSplit, EscrowReleased};
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
    /// Boxed like almost every account here, and for the same reason: this
    /// struct's generated `try_accounts` sits close enough to SBF's 4 KB
    /// stack-frame limit that adding one more account overflowed it. The
    /// symptom was not a compile failure — `cargo build-sbf` reports
    /// "overwrites values in the frame" as a non-fatal `Error:` line and
    /// still produces a binary — but an "Access violation ... at address
    /// 0x0" at runtime, on the *decided* path, nowhere near the account
    /// that was added. Keep new accounts here boxed.
    #[account(mint::token_program = token_program)]
    pub mint: Box<InterfaceAccount<'info, Mint>>,

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
    /// `mint`, which is the settlement stablecoin, and pinned to its own
    /// `deposit_token_program` for that reason.
    #[account(
        constraint = deposit_mint.key() == dispute_case.deposit_mint,
        mint::token_program = deposit_token_program,
    )]
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

    /// Moves the settlement stablecoin: the escrow release, the unwind back
    /// to the liquidity vault, and the even split.
    pub token_program: Interface<'info, TokenInterface>,

    /// Moves the OPEN arbitration deposit, and **must** be a separate handle
    /// from `token_program` above.
    ///
    /// This instruction is the one place in the workspace that transfers two
    /// different mints atomically, and a mint's owning token program is fixed
    /// when the mint is created. OPEN is Token-2022; the settlement
    /// stablecoin is frequently legacy SPL (devnet wSOL and USDC both are).
    /// A single shared handle would therefore be provably wrong for one of
    /// the two transfers in the normal production pairing — and wrong at CPI
    /// time, deep inside `settle_deposit`, rather than at account validation
    /// where it could be read off an error. That would have made the entire
    /// dispute-resolution path uncallable for exactly the mints this
    /// migration exists to support, while every Token-2022-only test kept
    /// passing. See [`openfiat_programs_shared::token_dispatch`].
    ///
    /// The two may legitimately be the *same* program id when both mints
    /// happen to share one — that is a coincidence of configuration, not
    /// something to collapse the accounts over.
    pub deposit_token_program: Interface<'info, TokenInterface>,

    /// CHECK: pinned to the real sysvar by `address`, then read as raw
    /// bytes — see `shared_logic::latch_case_seed`.
    ///
    /// Needed even though this instruction may not re-open the case: a
    /// round that decides never touches it, but a round that falls short
    /// must draw a fresh seed, and Anchor's account list is fixed per
    /// instruction rather than per branch. Passing it always is the cost of
    /// the seed being re-drawn at all.
    #[account(address = SlotHashes::id())]
    pub slot_hashes: UncheckedAccount<'info>,
    //
    // `remaining_accounts[0]`, optional: the singleton
    // [`ArbitrationPolicy`]. Supplies the eligible-arbitrator count the pool
    // floor is checked against — see [`published_pool_size`] for why it is a
    // remaining account rather than a field here, and OFS-4100 Annex A for
    // what it is used for.
}

/// Reads governance's published eligible-arbitrator count, if the caller
/// supplied it.
///
/// # Why this is a remaining account and not a field above
///
/// Two reasons, and the first is the one that decided it. This struct's
/// generated `try_accounts` already sits close enough to SBF's 4 KB
/// stack-frame limit that adding an account overflowed it once — see the
/// note on `mint`. `remaining_accounts` are handed over as a slice and never
/// enter that expansion, so reading one costs the frame nothing.
///
/// The second is that it makes the account genuinely optional, and optional
/// is the correct shape. `execute_dispute_outcome` is permissionless and
/// must stay callable on a cluster where `publish_arbitrator_pool_size` has
/// never run — which is every cluster today. A required field would make
/// every dispute unresolvable until an operator remembered to create the
/// policy account.
///
/// # What that costs, stated plainly
///
/// A caller can withhold the account and suppress the floor. That is a real
/// limitation and it is bounded: the floor never changes a payout, only how
/// soon the terminal split arrives and whether the reason is recorded. A
/// party who withholds it buys extra rounds of a frozen escrow, which is
/// precisely the status quo this change improves on and not a regression
/// from it. Anyone — the counterparty, an indexer, a keeper — can call with
/// the account and get the recorded early exit.
///
/// Supplying something *else* is not possible: the address must be the
/// canonical PDA, and Anchor's own checks reject an account this program
/// does not own or that does not carry `ArbitrationPolicy`'s discriminator.
fn published_pool_size<'info>(remaining: &'info [AccountInfo<'info>]) -> Result<u32> {
    let Some(info) = remaining.first() else {
        return Ok(0);
    };
    let (expected, _) = Pubkey::find_program_address(&[ARBITRATION_POLICY_SEED], &crate::ID);
    require_keys_eq!(info.key(), expected, ErrorCode::InvalidArbitrationPolicy);
    let policy: Account<ArbitrationPolicy> = Account::try_from(info)?;
    Ok(policy.eligible_arbitrators)
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
        // `deposit_token_program`, not `token_program`: this transfer is
        // OPEN-denominated, and OPEN's owning program need not be the
        // settlement stablecoin's.
        transfer_checked(
            CpiContext::new_with_signer(
                ctx.accounts.deposit_token_program.key(),
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

/// Sums each revealed vote's weight per outcome and returns the winner —
/// or `None` when the round reached no decision — together with how many
/// reveals were actually counted.
///
/// The count comes back rather than staying private because an undecided
/// round has to say *why* it was undecided, and "fewer than
/// [`MIN_ARBITRATORS`] reveals were counted" and "the weights tied" are
/// different failures with different causes. Recomputing it at the call site
/// would mean two places encoding the zero-weight rule, which is the one part
/// of this tally an attacker has already tried to exploit.
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
fn tally(dispute_case: &DisputeCase) -> (Option<DisputeOutcome>, usize) {
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
        return (None, counted);
    }

    let max = *totals.iter().max().unwrap();
    let winners: Vec<usize> = totals
        .iter()
        .enumerate()
        .filter(|(_, t)| **t == max)
        .map(|(i, _)| i)
        .collect();
    if winners.len() != 1 {
        return (None, counted);
    }
    let outcome = match winners[0] {
        0 => DisputeOutcome::BuyerWins,
        1 => DisputeOutcome::MerchantWins,
        2 => DisputeOutcome::MutualSettlement,
        _ => DisputeOutcome::InvalidDispute,
    };
    (Some(outcome), counted)
}

pub fn handle_execute_dispute_outcome(mut ctx: Context<ExecuteDisputeOutcome>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    require!(
        now >= ctx.accounts.dispute_case.reveal_deadline,
        ErrorCode::RevealWindowStillOpen
    );

    let (outcome, counted_reveals) = tally(&ctx.accounts.dispute_case);
    let Some(outcome) = outcome else {
        return handle_undecided_round(ctx, now, counted_reveals);
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
///
/// # A round the pool cannot staff is not re-opened (OFS-4100 Annex A)
///
/// Barring and the round budget compose into a floor on the arbitrator pool
/// that neither of them states: a case is only decidable if the eligible pool
/// holds `MIN_ARBITRATORS + MAX_BARRED_ARBITRATORS` wallets. Below that the
/// final round cannot reach quorum however honestly everyone behaves, so the
/// case lands on the terminal even split — which is exactly what the party
/// facing a losing verdict was trying to buy. Bouncing twice more first
/// changes nothing about that except how long the escrow stays frozen and how
/// hard the condition is to see from outside.
///
/// So when the pool provably cannot staff another round, this stops there and
/// says so. The split is identical; what is new is that it is recorded as
/// [`TerminalSplitReason::PoolExhausted`] rather than being indistinguishable
/// from arbitrators who genuinely disagreed. Every terminal split now carries
/// a reason, including on deployments that publish no pool size at all — see
/// [`PoolFloor::exhausted_rounds_reason`], which reads only the case's own
/// bookkeeping.
fn handle_undecided_round(
    mut ctx: Context<ExecuteDisputeOutcome>,
    now: i64,
    counted_reveals: usize,
) -> Result<()> {
    let next_round = ctx
        .accounts
        .dispute_case
        .round
        .checked_add(1)
        .ok_or(ErrorCode::Overflow)?;

    let seats_this_round = ctx.accounts.dispute_case.arbitrators.len();
    // Retire this round's silent seats before anything is decided. What they
    // cost the case is the input to the pool floor below, not a consequence
    // of it — and the arrays that record who was silent are cleared the
    // moment a new round opens.
    bar_silent_seats(&mut ctx.accounts.dispute_case);

    let floor = PoolFloor {
        barred: ctx.accounts.dispute_case.barred.len() as u32,
        seats_taken_total: ctx.accounts.dispute_case.seats_taken_total,
        seats_this_round: seats_this_round as u32,
        counted_reveals: counted_reveals as u32,
        published_pool: published_pool_size(ctx.remaining_accounts)?,
    };

    let reason = if next_round >= MAX_DISPUTE_ROUNDS {
        floor.exhausted_rounds_reason()
    } else if floor.next_round_is_staffable() {
        return reopen_round(ctx, now, next_round);
    } else {
        TerminalSplitReason::PoolExhausted
    };

    // Nobody was found at fault, so the deposit goes back to the merchant
    // rather than being forfeited — see `settle_deposit`.
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
    case.terminal_reason = Some(reason);

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
    // Alongside, never instead of, the event above: that one says what
    // happened to the money, this one says why arbitration produced no
    // verdict. See [`DisputeTerminalSplit`].
    emit!(DisputeTerminalSplit {
        reservation_id: case.reservation_id,
        reason,
        round: case.round,
        seats_this_round: seats_this_round as u8,
        counted_reveals: counted_reveals as u8,
        barred: floor.barred as u8,
        seats_taken_total: floor.seats_taken_total,
        required_pool: floor.required_for_next_round(),
        published_pool: floor.published_pool,
        timestamp: now,
    });
    Ok(())
}

/// Retires every seat that committed this round and never revealed.
///
/// Whoever took a seat and stayed silent loses it for the rest of the case.
/// Run before the per-round arrays are cleared, because after that there is
/// no record of who was silent — and a fresh draw alone does not stop a stake
/// large enough to qualify from qualifying again.
///
/// # The cap has to be counted as it fills
///
/// `barred` is a `#[max_len(MAX_BARRED_ARBITRATORS)]` vector, so pushing past
/// that capacity does not truncate — it makes the account fail to serialize,
/// which would leave the escrow frozen with no instruction able to move it.
/// The guard therefore counts what this round is *about* to add as well as
/// what is already stored. Reading only the stored length, as it did, meant a
/// round starting at 13 barred could push 7 more and reach 20: unreachable
/// while `MAX_DISPUTE_ROUNDS` is 3 and only two rounds ever bar anyone, but
/// live the moment either of those facts changes — which is exactly what
/// happened when the terminal round started barring too.
fn bar_silent_seats(dispute_case: &mut DisputeCase) {
    let mut barred_this_round: Vec<Pubkey> = Vec::new();
    for (index, arbitrator) in dispute_case.arbitrators.iter().enumerate() {
        if dispute_case.barred.len() + barred_this_round.len() >= MAX_BARRED_ARBITRATORS {
            break;
        }
        let revealed = dispute_case
            .revealed_outcomes
            .get(index)
            .is_some_and(|outcome| outcome.is_some());
        if !revealed && !dispute_case.barred.contains(arbitrator) {
            barred_this_round.push(*arbitrator);
        }
    }
    dispute_case.barred.extend(barred_this_round);
}

/// Re-opens the case for another round: fresh deadlines, a fresh draw, and an
/// empty bench.
fn reopen_round(ctx: Context<ExecuteDisputeOutcome>, now: i64, next_round: u8) -> Result<()> {
    let seed = crate::instructions::shared_logic::latch_case_seed(
        &ctx.accounts.slot_hashes.to_account_info(),
        ctx.accounts.dispute_case.reservation_id,
        &ctx.accounts.dispute_case.trade_escrow,
    )?;

    let dispute_case = &mut ctx.accounts.dispute_case;
    let commit_deadline = now
        .checked_add(dispute_case.commit_window_secs)
        .ok_or(ErrorCode::Overflow)?;
    let reveal_deadline = commit_deadline
        .checked_add(dispute_case.reveal_window_secs)
        .ok_or(ErrorCode::Overflow)?;

    dispute_case.round = next_round;
    dispute_case.round_opened_at = now;
    dispute_case.commit_deadline = commit_deadline;
    dispute_case.reveal_deadline = reveal_deadline;
    // A fresh draw for the fresh round. Carrying the previous round's seed
    // over would mean exactly the same wallets qualify again, so an
    // attacker who won the draw once would hold those seats for every
    // remaining round — and forcing a re-round is something they can do
    // deliberately by committing and never revealing.
    dispute_case.case_seed = seed;

    dispute_case.arbitrators = Vec::new();
    dispute_case.commitments = Vec::new();
    dispute_case.revealed_outcomes = Vec::new();
    dispute_case.weights = Vec::new();
    dispute_case.reward_claimed = Vec::new();
    Ok(())
}
