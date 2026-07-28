use anchor_lang::prelude::*;

use crate::{constants::*, error::ErrorCode, state::*};

/// Permissionless, callable once voting has ended (OFS-4200 §6) — matches
/// `openfiat-presale`'s `finalize_sale` and `openfiat-escrow`'s
/// `expire_reservation`/`execute_dispute_outcome` permissionless-after-a-
/// deadline pattern. A quorum miss or a genuine vote tie both resolve to
/// `Rejected`, deterministically — no discretionary judgment call.
#[derive(Accounts)]
pub struct TallyAndFinalize<'info> {
    #[account(
        mut,
        seeds = [PROPOSAL_SEED, &proposal.id.to_le_bytes()],
        bump = proposal.bump,
        constraint = proposal.state == ProposalState::Voting @ ErrorCode::NotInVotingState,
    )]
    pub proposal: Account<'info, Proposal>,
}

pub fn handle_tally_and_finalize(ctx: Context<TallyAndFinalize>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let proposal = &mut ctx.accounts.proposal;
    require!(now >= proposal.voting_ends_at, ErrorCode::VotingStillOpen);

    let total_cast = proposal
        .votes_for
        .checked_add(proposal.votes_against)
        .ok_or(ErrorCode::Overflow)?;
    let quorum_met = total_cast >= proposal.quorum_snapshot;
    proposal.quorum_met = quorum_met;

    let accepted = if !quorum_met || total_cast == 0 {
        false
    } else {
        let for_bps = (proposal.votes_for as u128)
            .checked_mul(BPS_DENOMINATOR as u128)
            .ok_or(ErrorCode::Overflow)?
            .checked_div(total_cast as u128)
            .ok_or(ErrorCode::Overflow)?;
        for_bps >= proposal.threshold_snapshot as u128
    };

    proposal.state = if accepted {
        ProposalState::Accepted
    } else {
        ProposalState::Rejected
    };
    Ok(())
}
