use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    transfer_checked, Mint, TokenAccount, TokenInterface, TransferChecked,
};

use crate::{constants::*, error::ErrorCode, events::ProposalDepositSettled, state::*};

/// Permissionless, callable once `tally_and_finalize` has run. Refunds
/// the proposer if quorum was met (regardless of accept/reject — OFS-4100
/// §5: "refunded if quorum is met by the voting deadline, independent of
/// whether the proposal itself passes"), otherwise forfeits to
/// `GovernanceConfig.forfeit_destination`.
///
/// Every deserialized account here is `Box`ed. Anchor builds the whole
/// struct on the BPF stack, which is a hard 4KB per frame, and this
/// context carries three token accounts, a mint and two program accounts
/// — enough that adding a single 32-byte field to `Proposal` pushed
/// `try_accounts` eight bytes past the limit and failed the build.
/// Boxing moves the bodies to the heap and leaves pointers on the stack,
/// so the frame no longer grows with the account layouts. Done to all of
/// them rather than just enough to squeak under, because "enough" is a
/// number the next field would break again.
#[derive(Accounts)]
pub struct RefundOrForfeitDeposit<'info> {
    #[account(mint::token_program = token_program)]
    pub mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(
        seeds = [GOVERNANCE_CONFIG_SEED],
        bump = governance_config.bump,
        constraint = governance_config.mint == mint.key(),
        constraint = governance_config.forfeit_destination == forfeit_destination.key(),
    )]
    pub governance_config: Box<Account<'info, GovernanceConfig>>,

    #[account(mut, seeds = [DEPOSIT_VAULT_SEED], bump = governance_config.deposit_vault_bump)]
    pub deposit_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        mut,
        seeds = [PROPOSAL_SEED, &proposal.id.to_le_bytes()],
        bump = proposal.bump,
        constraint = proposal.state != ProposalState::Voting @ ErrorCode::NotYetTallied,
        constraint = !proposal.deposit_settled @ ErrorCode::DepositAlreadySettled,
    )]
    pub proposal: Box<Account<'info, Proposal>>,

    #[account(mut, constraint = proposer_token_account.owner == proposal.proposer, constraint = proposer_token_account.mint == mint.key())]
    pub proposer_token_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut)]
    pub forfeit_destination: Box<InterfaceAccount<'info, TokenAccount>>,

    pub token_program: Interface<'info, TokenInterface>,
}

pub fn handle_refund_or_forfeit_deposit(ctx: Context<RefundOrForfeitDeposit>) -> Result<()> {
    let amount = ctx.accounts.proposal.stake_deposit;
    // Quorum alone decides this, not acceptance — OFS-4100 §5. The two
    // are emitted together below so the branch is checkable from the log.
    let refunded = ctx.accounts.proposal.quorum_met;
    let destination = if refunded {
        ctx.accounts.proposer_token_account.to_account_info()
    } else {
        ctx.accounts.forfeit_destination.to_account_info()
    };
    let destination_key = destination.key();

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

    let proposal = &ctx.accounts.proposal;
    emit!(ProposalDepositSettled {
        proposal: proposal.key(),
        proposal_id: proposal.id,
        proposer: proposal.proposer,
        amount,
        mint: ctx.accounts.mint.key(),
        refunded,
        destination: destination_key,
        quorum_met: proposal.quorum_met,
        state: proposal.state,
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}
