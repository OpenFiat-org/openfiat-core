use anchor_lang::prelude::*;
use openfiat_programs_shared::{DisputeOutcome, Role};
use sha2::{Digest, Sha256};

use crate::{constants::*, error::ErrorCode, state::*};

#[derive(Accounts)]
pub struct RevealDisputeVote<'info> {
    pub arbitrator: Signer<'info>,

    #[account(
        mut,
        seeds = [DISPUTE_CASE_SEED, &dispute_case.reservation_id.to_le_bytes()],
        bump = dispute_case.bump,
    )]
    pub dispute_case: Account<'info, DisputeCase>,

    /// Read for the Arbitrator-role minimum, so a stake that has fallen
    /// below it — a slash between commit and reveal can do that — weighs
    /// zero rather than weighing its full face value. See
    /// `staking::StakeAccount::effective_stake`.
    #[account(
        seeds = [staking::STAKING_CONFIG_SEED],
        seeds::program = staking::ID,
        bump = staking_config.bump,
    )]
    pub staking_config: Account<'info, staking::StakingConfig>,

    /// This arbitrator's own Arbitrator-role stake — read directly (no
    /// CPI dispatch) per `staking::StakeAccount::effective_stake`'s own
    /// doc comment. Seeds pin the role to `Arbitrator`, so a wallet with
    /// no arbitrator stake registered simply cannot supply a valid
    /// account here.
    #[account(
        seeds = [staking::STAKE_ACCOUNT_SEED, arbitrator.key().as_ref(), &[Role::Arbitrator as u8]],
        seeds::program = staking::ID,
        bump = arbitrator_stake.bump,
    )]
    pub arbitrator_stake: Account<'info, staking::StakeAccount>,
}

pub fn handle_reveal_dispute_vote(
    ctx: Context<RevealDisputeVote>,
    outcome: DisputeOutcome,
    salt: [u8; 32],
) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let dispute_case = &mut ctx.accounts.dispute_case;

    require!(
        now >= dispute_case.commit_deadline && now < dispute_case.reveal_deadline,
        ErrorCode::NotInRevealWindow
    );

    let arbitrator = ctx.accounts.arbitrator.key();
    let index = dispute_case
        .arbitrators
        .iter()
        .position(|a| *a == arbitrator)
        .ok_or(ErrorCode::NoCommitmentFound)?;
    require!(
        dispute_case.revealed_outcomes[index].is_none(),
        ErrorCode::AlreadyRevealed
    );

    let mut hasher = Sha256::new();
    hasher.update([outcome as u8]);
    hasher.update(salt);
    let computed: [u8; 32] = hasher.finalize().into();
    require!(
        computed == dispute_case.commitments[index],
        ErrorCode::CommitmentMismatch
    );

    dispute_case.revealed_outcomes[index] = Some(outcome);
    dispute_case.weights[index] = ctx
        .accounts
        .arbitrator_stake
        .effective_stake(&ctx.accounts.staking_config);
    Ok(())
}
