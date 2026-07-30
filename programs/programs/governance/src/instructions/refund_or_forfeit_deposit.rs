use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    transfer_checked, Mint, TokenAccount, TokenInterface, TransferChecked,
};

use crate::{constants::*, error::ErrorCode, state::*};

/// Permissionless, callable once `tally_and_finalize` has run. Refunds
/// the proposer if quorum was met (regardless of accept/reject — OFS-4100
/// §5: "refunded if quorum is met by the voting deadline, independent of
/// whether the proposal itself passes"), otherwise forfeits to
/// `GovernanceConfig.forfeit_destination`.
#[derive(Accounts)]
pub struct RefundOrForfeitDeposit<'info> {
    #[account(mint::token_program = token_program)]
    pub mint: InterfaceAccount<'info, Mint>,

    #[account(
        seeds = [GOVERNANCE_CONFIG_SEED],
        bump = governance_config.bump,
        constraint = governance_config.mint == mint.key(),
        constraint = governance_config.forfeit_destination == forfeit_destination.key(),
    )]
    pub governance_config: Account<'info, GovernanceConfig>,

    #[account(mut, seeds = [DEPOSIT_VAULT_SEED], bump = governance_config.deposit_vault_bump)]
    pub deposit_vault: InterfaceAccount<'info, TokenAccount>,

    #[account(
        mut,
        seeds = [PROPOSAL_SEED, &proposal.id.to_le_bytes()],
        bump = proposal.bump,
        constraint = proposal.state != ProposalState::Voting @ ErrorCode::NotYetTallied,
        constraint = !proposal.deposit_settled @ ErrorCode::DepositAlreadySettled,
    )]
    pub proposal: Account<'info, Proposal>,

    #[account(mut, constraint = proposer_token_account.owner == proposal.proposer, constraint = proposer_token_account.mint == mint.key())]
    pub proposer_token_account: InterfaceAccount<'info, TokenAccount>,

    #[account(mut)]
    pub forfeit_destination: InterfaceAccount<'info, TokenAccount>,

    pub token_program: Interface<'info, TokenInterface>,
}

pub fn handle_refund_or_forfeit_deposit(ctx: Context<RefundOrForfeitDeposit>) -> Result<()> {
    let amount = ctx.accounts.proposal.stake_deposit;
    let destination = if ctx.accounts.proposal.quorum_met {
        ctx.accounts.proposer_token_account.to_account_info()
    } else {
        ctx.accounts.forfeit_destination.to_account_info()
    };

    let bump = ctx.accounts.governance_config.bump;
    let signer_seeds: &[&[u8]] = &[GOVERNANCE_CONFIG_SEED, &[bump]];

    transfer_checked(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: ctx.accounts.deposit_vault.to_account_info(),
                mint: ctx.accounts.mint.to_account_info(),
                to: destination,
                authority: ctx.accounts.governance_config.to_account_info(),
            },
            &[signer_seeds],
        ),
        amount,
        ctx.accounts.mint.decimals,
    )?;

    ctx.accounts.proposal.deposit_settled = true;
    Ok(())
}
