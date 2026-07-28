use anchor_lang::prelude::*;

use crate::{constants::*, error::ErrorCode, state::*};

/// Any wallet may commit — off-chain eligibility (minimum stake,
/// reputation, protocol age, no active penalties per Chapter 11 §11.6)
/// is `crates/disputes::join_as_arbitrator`'s job before this ever gets
/// relayed on-chain; this instruction faithfully records an
/// already-vetted arbitrator's commitment, weighting their vote by real
/// stake later at reveal time rather than gating commit-time
/// eligibility a second way on-chain.
#[derive(Accounts)]
pub struct CommitDisputeVote<'info> {
    pub arbitrator: Signer<'info>,

    #[account(
        mut,
        seeds = [DISPUTE_CASE_SEED, &dispute_case.reservation_id.to_le_bytes()],
        bump = dispute_case.bump,
    )]
    pub dispute_case: Account<'info, DisputeCase>,
}

pub fn handle_commit_dispute_vote(
    ctx: Context<CommitDisputeVote>,
    commitment: [u8; 32],
) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let dispute_case = &mut ctx.accounts.dispute_case;

    require!(
        now < dispute_case.commit_deadline,
        ErrorCode::CommitWindowClosed
    );
    require!(
        !dispute_case
            .arbitrators
            .contains(&ctx.accounts.arbitrator.key()),
        ErrorCode::AlreadyCommitted
    );
    require!(
        dispute_case.arbitrators.len() < MAX_ARBITRATORS,
        ErrorCode::DisputeCaseFull
    );

    dispute_case.arbitrators.push(ctx.accounts.arbitrator.key());
    dispute_case.commitments.push(commitment);
    dispute_case.revealed_outcomes.push(None);
    dispute_case.weights.push(0);
    Ok(())
}
