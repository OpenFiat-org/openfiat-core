use anchor_lang::prelude::*;
use openfiat_programs_shared::Role;

use crate::{constants::*, error::ErrorCode, events::VoteCast, state::*};

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

    /// Read for `role`'s minimum stake, so a balance that has fallen
    /// below it carries no voting weight. A slash can leave one there:
    /// `stake`/`request_unstake` refuse to create a below-minimum
    /// balance, `slash` does not. See
    /// `staking::StakeAccount::effective_stake`.
    #[account(
        seeds = [staking::STAKING_CONFIG_SEED],
        seeds::program = staking::ID,
        bump = staking_config.bump,
    )]
    pub staking_config: Account<'info, staking::StakingConfig>,

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

pub fn handle_cast_vote(ctx: Context<CastVote>, in_favor: bool, role: Role) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    require!(
        now < ctx.accounts.proposal.voting_ends_at,
        ErrorCode::NotInVotingState
    );

    // The one and only weight in this instruction: the voter's on-chain
    // stake, reduced to zero below the role minimum. `cast_vote` takes no
    // weight argument, so there is no self-reported figure that could be
    // counted here or emitted below — see `VoteCast`'s own doc.
    let weight = ctx
        .accounts
        .voter_stake
        .effective_stake(&ctx.accounts.staking_config);
    let voter_stake_key = ctx.accounts.voter_stake.key();

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

    let proposal_key = proposal.key();
    let proposal_id = proposal.id;
    let votes_for = proposal.votes_for;
    let votes_against = proposal.votes_against;

    let locked_until = now
        .checked_add(ctx.accounts.governance_config.vote_lock_secs)
        .ok_or(ErrorCode::Overflow)?;

    let vote_record = &mut ctx.accounts.vote_record;
    vote_record.proposal = proposal_key;
    vote_record.voter = ctx.accounts.voter.key();
    vote_record.weight = weight;
    vote_record.in_favor = in_favor;
    vote_record.locked_until = locked_until;
    vote_record.bump = ctx.bumps.vote_record;

    emit!(VoteCast {
        proposal: proposal_key,
        proposal_id,
        voter: vote_record.voter,
        voter_stake: voter_stake_key,
        role,
        in_favor,
        weight,
        votes_for,
        votes_against,
        locked_until,
        timestamp: now,
    });
    Ok(())
}
