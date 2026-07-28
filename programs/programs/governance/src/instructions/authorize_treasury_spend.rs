use anchor_lang::prelude::*;

use crate::{constants::*, error::ErrorCode, state::*};

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
    _destination: Pubkey,
    _amount: u64,
) -> Result<()> {
    ctx.accounts.proposal.executed = true;
    Ok(())
}
