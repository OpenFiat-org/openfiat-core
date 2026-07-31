use anchor_lang::prelude::*;

use crate::{constants::*, error::ErrorCode, events::TreasurySpendAuthorized, state::*};

/// OFS-4200 §6: `authorize_treasury_spend`, callable only when `Accepted`
/// and `category == Treasury`.
///
/// **Scoped for this phase**, same rationale as `update_config_parameter`:
/// records the authorization (`Proposal.executed = true`) rather than
/// moving funds. `openfiat-escrow::FeeConfig`'s treasuries are plain
/// destination token accounts owned by external wallets, not a program-
/// controlled vault this program could CPI into to disburse from — per
/// OFS-4200 §1, governance itself holds "no asset custody beyond small
/// proposal-stake deposits". A real treasury-spend vault + disbursement
/// instruction is future work once that custody model is designed.
#[derive(Accounts)]
pub struct AuthorizeTreasurySpend<'info> {
    #[account(
        mut,
        seeds = [PROPOSAL_SEED, &proposal.id.to_le_bytes()],
        bump = proposal.bump,
        constraint = proposal.state == ProposalState::Accepted @ ErrorCode::ProposalNotAccepted,
        constraint = proposal.category == openfiat_programs_shared::ProposalCategory::Treasury @ ErrorCode::WrongCategoryForTreasurySpend,
        constraint = !proposal.executed @ ErrorCode::AlreadyExecuted,
    )]
    pub proposal: Account<'info, Proposal>,
}

pub fn handle_authorize_treasury_spend(
    ctx: Context<AuthorizeTreasurySpend>,
    destination: Pubkey,
    amount: u64,
) -> Result<()> {
    ctx.accounts.proposal.executed = true;

    // `authorized_destination`/`authorized_amount`, not `destination`/
    // `amount`: no transfer happened, and an event that read as a
    // completed disbursement would have every explorer report protocol
    // funds leaving an account they never left. What is recorded is the
    // authorization — which until now was discarded, leaving an accepted
    // Treasury proposal with no on-chain trace of what it had approved.
    emit!(TreasurySpendAuthorized {
        proposal: ctx.accounts.proposal.key(),
        proposal_id: ctx.accounts.proposal.id,
        authorized_destination: destination,
        authorized_amount: amount,
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}
