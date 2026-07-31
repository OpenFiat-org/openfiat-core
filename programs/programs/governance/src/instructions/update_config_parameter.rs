use anchor_lang::prelude::*;

use crate::{constants::*, error::ErrorCode, events::ConfigParameterChangeAuthorized, state::*};

/// OFS-4200 §6: `update_config_parameter(target_program, parameter_key,
/// new_value)`, callable only when the calling proposal is `Accepted`
/// and `category == Parameter`.
///
/// **Scoped for this phase**: this records that the accepted proposal's
/// parameter change is authorized (`Proposal.executed = true`) — it does
/// not perform a live cross-program mutation. Actually changing e.g.
/// `escrow::FeeConfig.settlement_fee_bps` would need a real CPI into an
/// admin-gated update instruction on that program, and no such
/// instruction exists yet in `openfiat-escrow`/`openfiat-staking` (both
/// are still `admin`-only, not governance-PDA-aware). Wiring each
/// program's config to accept this program's PDA as an alternate
/// authority is real follow-up work (naturally Phase 6's job), not
/// something to fake here with an instruction that looks live but isn't.
#[derive(Accounts)]
pub struct UpdateConfigParameter<'info> {
    #[account(
        mut,
        seeds = [PROPOSAL_SEED, &proposal.id.to_le_bytes()],
        bump = proposal.bump,
        constraint = proposal.state == ProposalState::Accepted @ ErrorCode::ProposalNotAccepted,
        constraint = proposal.category == openfiat_programs_shared::ProposalCategory::Parameter @ ErrorCode::WrongCategoryForParameterUpdate,
        constraint = !proposal.executed @ ErrorCode::AlreadyExecuted,
    )]
    pub proposal: Account<'info, Proposal>,
}

pub fn handle_update_config_parameter(
    ctx: Context<UpdateConfigParameter>,
    target_program: Pubkey,
    parameter_key: String,
    new_value: u64,
) -> Result<()> {
    ctx.accounts.proposal.executed = true;

    // Named `ConfigParameterChangeAuthorized`, with an `authorized_value`
    // rather than a `new_value`, because nothing was written: the three
    // arguments were previously discarded entirely, so an accepted
    // Parameter proposal left no on-chain record of *what* it had
    // authorized. Recording them is the point; implying they took effect
    // would be worse than the silence it replaces.
    emit!(ConfigParameterChangeAuthorized {
        proposal: ctx.accounts.proposal.key(),
        proposal_id: ctx.accounts.proposal.id,
        target_program,
        parameter_key,
        authorized_value: new_value,
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}
