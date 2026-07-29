use anchor_lang::prelude::*;
use openfiat_programs_shared::VaultState;

use crate::{constants::*, error::ErrorCode, state::*};

/// Opens a dispute case and freezes the trade escrow in one atomic step
/// (Phase 4b, plan decision #2) — replaces Phase 4's standalone
/// `freeze_on_dispute`/`dispute_authority` design, which trusted an
/// external signer with no on-chain proof of a real dispute. Callable by
/// either party to the trade, matching OFS-2400 §5 ("a dispute MAY be
/// initiated when... buyer disagrees... merchant reports...").
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
    pub dispute_case: Account<'info, DisputeCase>,

    pub system_program: Program<'info, System>,
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

    let dispute_case = &mut ctx.accounts.dispute_case;
    dispute_case.reservation_id = ctx.accounts.trade_escrow.reservation_id;
    dispute_case.trade_escrow = ctx.accounts.trade_escrow.key();
    dispute_case.opened_at = now;
    dispute_case.commit_deadline = commit_deadline;
    dispute_case.reveal_deadline = reveal_deadline;
    dispute_case.resolved = false;
    dispute_case.round = 0;
    dispute_case.commit_window_secs = commit_window_secs;
    dispute_case.reveal_window_secs = reveal_window_secs;
    dispute_case.arbitrators = Vec::new();
    dispute_case.commitments = Vec::new();
    dispute_case.revealed_outcomes = Vec::new();
    dispute_case.weights = Vec::new();
    dispute_case.bump = ctx.bumps.dispute_case;

    ctx.accounts.trade_escrow.state = VaultState::Frozen;
    Ok(())
}
