use anchor_lang::prelude::*;
use openfiat_programs_shared::Role;

use crate::{constants::*, error::ErrorCode, state::*};

/// Records one arbitrator's sealed commitment for a dispute case.
///
/// Committing is gated on the wallet actually holding the Arbitrator
/// role's minimum stake. It previously was not — the reasoning being that
/// off-chain eligibility (`crates/disputes::join_as_arbitrator`) vetted
/// arbitrators before anything was relayed on-chain, and that a vote gets
/// weighted by real stake at reveal time anyway.
///
/// Both halves of that were wrong. Nothing forces a commit to arrive via
/// the off-chain path — this instruction is reachable directly — and
/// weighting at reveal does not help when the seats themselves are the
/// scarce resource. `initialize_stake_account` is permissionless and a
/// zero-balance account is a legal one, so seven throwaway wallets could
/// occupy all [`MAX_ARBITRATORS`] slots for the price of rent, reveal
/// zero-weight votes, and drive every outcome total to zero. That tie
/// resolved to `InvalidDispute`, which returns the escrow to the seller:
/// a merchant could keep a buyer's money after the buyer had already sent
/// fiat, risking no OPEN at all.
///
/// Requiring the stake at commit time makes filling every seat cost
/// `MAX_ARBITRATORS` × the arbitrator minimum in genuinely slashable
/// stake rather than rent.
#[derive(Accounts)]
pub struct CommitDisputeVote<'info> {
    pub arbitrator: Signer<'info>,

    #[account(
        mut,
        seeds = [DISPUTE_CASE_SEED, &dispute_case.reservation_id.to_le_bytes()],
        bump = dispute_case.bump,
    )]
    pub dispute_case: Account<'info, DisputeCase>,

    /// Read for the current Arbitrator-role minimum, so the gate follows a
    /// governance parameter change with no redeploy here.
    #[account(
        seeds = [staking::STAKING_CONFIG_SEED],
        seeds::program = staking::ID,
        bump = staking_config.bump,
    )]
    pub staking_config: Account<'info, staking::StakingConfig>,

    /// This arbitrator's own Arbitrator-role stake. Same constraint style
    /// as `reveal_dispute_vote`: the seeds pin both owner and role, so
    /// neither another wallet's stake nor a different role's can be
    /// substituted.
    #[account(
        seeds = [staking::STAKE_ACCOUNT_SEED, arbitrator.key().as_ref(), &[Role::Arbitrator as u8]],
        seeds::program = staking::ID,
        bump = arbitrator_stake.bump,
    )]
    pub arbitrator_stake: Account<'info, staking::StakeAccount>,
}

pub fn handle_commit_dispute_vote(
    ctx: Context<CommitDisputeVote>,
    commitment: [u8; 32],
) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;

    require!(
        ctx.accounts
            .arbitrator_stake
            .effective_stake(&ctx.accounts.staking_config)
            >= ctx.accounts.staking_config.min_stake_for(Role::Arbitrator),
        ErrorCode::ArbitratorStakeBelowMinimum
    );

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
