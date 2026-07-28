use anchor_lang::prelude::*;
use openfiat_programs_shared::Role;

use crate::{constants::*, error::ErrorCode, state::*};

#[derive(Accounts)]
#[instruction(in_favor: bool, role: Role)]
pub struct CastVote<'info> {
    #[account(mut)]
    pub voter: Signer<'info>,

    #[account(
        seeds = [GOVERNANCE_CONFIG_SEED],
        bump = governance_config.bump,
    )]
    pub governance_config: Account<'info, GovernanceConfig>,

    #[account(
        mut,
        seeds = [PROPOSAL_SEED, &proposal.id.to_le_bytes()],
        bump = proposal.bump,
        constraint = proposal.state == ProposalState::Voting @ ErrorCode::NotInVotingState,
    )]
    pub proposal: Account<'info, Proposal>,

    /// This voter's stake under `role` — any role counts toward voting
    /// weight (unlike Phase 4b's dispute-vote reveal, which is
    /// deliberately Arbitrator-only). A voter holding stake under
    /// several roles picks one per proposal; `VoteRecord`'s PDA (keyed
    /// by proposal+voter only, not role) is what actually enforces one
    /// vote per proposal regardless of how many roles they hold.
    #[account(
        seeds = [staking::STAKE_ACCOUNT_SEED, voter.key().as_ref(), &[role as u8]],
        seeds::program = staking::ID,
        bump = voter_stake.bump,
    )]
    pub voter_stake: Account<'info, staking::StakeAccount>,

    #[account(
        init,
        payer = voter,
        space = 8 + VoteRecord::INIT_SPACE,
        seeds = [VOTE_RECORD_SEED, proposal.key().as_ref(), voter.key().as_ref()],
        bump
    )]
    pub vote_record: Account<'info, VoteRecord>,

    pub system_program: Program<'info, System>,
}

pub fn handle_cast_vote(ctx: Context<CastVote>, in_favor: bool, _role: Role) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    require!(
        now < ctx.accounts.proposal.voting_ends_at,
        ErrorCode::NotInVotingState
    );

    let weight = ctx.accounts.voter_stake.effective_stake();

    let proposal = &mut ctx.accounts.proposal;
    if in_favor {
        proposal.votes_for = proposal
            .votes_for
            .checked_add(weight)
            .ok_or(ErrorCode::Overflow)?;
    } else {
        proposal.votes_against = proposal
            .votes_against
            .checked_add(weight)
            .ok_or(ErrorCode::Overflow)?;
    }

    let vote_record = &mut ctx.accounts.vote_record;
    vote_record.proposal = ctx.accounts.proposal.key();
    vote_record.voter = ctx.accounts.voter.key();
    vote_record.weight = weight;
    vote_record.in_favor = in_favor;
    vote_record.locked_until = now
        .checked_add(ctx.accounts.governance_config.vote_lock_secs)
        .ok_or(ErrorCode::Overflow)?;
    vote_record.bump = ctx.bumps.vote_record;
    Ok(())
}
