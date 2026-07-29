use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    transfer_checked, Mint, Token2022, TokenAccount, TransferChecked,
};
use openfiat_programs_shared::ProposalCategory;

use crate::shared_logic::{quorum_bps_for, threshold_bps_for};
use crate::{constants::*, error::ErrorCode, state::*};

#[derive(Accounts)]
#[instruction(id: u64)]
pub struct CreateProposal<'info> {
    #[account(mut)]
    pub proposer: Signer<'info>,

    /// CHECK: OFS-7100 §12 deposit gate, enforced by *proof of
    /// non-existence*. Unchecked and uninitialized on purpose — the
    /// wallet is banned iff this address is occupied, so in the passing
    /// case there is nothing to deserialize. The soundness lives in the
    /// constraint, not the type: `seeds`/`seeds::program` force this to
    /// be the one canonical ban address for `proposer` under
    /// `openfiat-governance`, so a banned caller cannot substitute an
    /// unrelated empty account and appear unbanned. Removing either line
    /// silently disables the ban for this instruction.
    #[account(
        seeds = [openfiat_programs_shared::BAN_SEED, proposer.key().as_ref()],
        bump,
        seeds::program = openfiat_programs_shared::GOVERNANCE_PROGRAM_ID,
    )]
    pub ban_record: UncheckedAccount<'info>,

    pub mint: InterfaceAccount<'info, Mint>,

    #[account(
        seeds = [GOVERNANCE_CONFIG_SEED],
        bump = governance_config.bump,
        constraint = governance_config.mint == mint.key(),
    )]
    pub governance_config: Account<'info, GovernanceConfig>,

    #[account(mut, seeds = [DEPOSIT_VAULT_SEED], bump = governance_config.deposit_vault_bump)]
    pub deposit_vault: InterfaceAccount<'info, TokenAccount>,

    #[account(mut, constraint = from.mint == mint.key())]
    pub from: InterfaceAccount<'info, TokenAccount>,

    #[account(
        init,
        payer = proposer,
        space = 8 + Proposal::INIT_SPACE,
        seeds = [PROPOSAL_SEED, &id.to_le_bytes()],
        bump
    )]
    pub proposal: Account<'info, Proposal>,

    pub token_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

pub fn handle_create_proposal(
    ctx: Context<CreateProposal>,
    id: u64,
    category: ProposalCategory,
    title_hash: [u8; 32],
    summary_hash: [u8; 32],
    voting_period_secs: i64,
) -> Result<()> {
    require!(
        !openfiat_programs_shared::wallet_is_banned(&ctx.accounts.ban_record),
        ErrorCode::WalletBanned
    );

    require!(voting_period_secs > 0, ErrorCode::InvalidVoteLock);

    let deposit_amount = ctx.accounts.governance_config.deposit_amount;
    transfer_checked(
        CpiContext::new(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: ctx.accounts.from.to_account_info(),
                mint: ctx.accounts.mint.to_account_info(),
                to: ctx.accounts.deposit_vault.to_account_info(),
                authority: ctx.accounts.proposer.to_account_info(),
            },
        ),
        deposit_amount,
        ctx.accounts.mint.decimals,
    )?;

    let quorum_bps = quorum_bps_for(&ctx.accounts.governance_config, category);
    let threshold_bps = threshold_bps_for(&ctx.accounts.governance_config, category);
    let quorum_snapshot = (ctx.accounts.governance_config.total_open_supply as u128)
        .checked_mul(quorum_bps as u128)
        .ok_or(ErrorCode::Overflow)?
        .checked_div(BPS_DENOMINATOR as u128)
        .ok_or(ErrorCode::Overflow)? as u64;

    let now = Clock::get()?.unix_timestamp;

    let proposal = &mut ctx.accounts.proposal;
    proposal.id = id;
    proposal.category = category;
    proposal.proposer = ctx.accounts.proposer.key();
    proposal.title_hash = title_hash;
    proposal.summary_hash = summary_hash;
    proposal.stake_deposit = deposit_amount;
    proposal.votes_for = 0;
    proposal.votes_against = 0;
    proposal.quorum_snapshot = quorum_snapshot;
    proposal.threshold_snapshot = threshold_bps;
    proposal.created_at = now;
    proposal.voting_ends_at = now
        .checked_add(voting_period_secs)
        .ok_or(ErrorCode::Overflow)?;
    proposal.state = ProposalState::Voting;
    proposal.quorum_met = false;
    proposal.deposit_settled = false;
    proposal.executed = false;
    proposal.bump = ctx.bumps.proposal;
    Ok(())
}
