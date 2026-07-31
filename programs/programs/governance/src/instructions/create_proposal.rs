use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    transfer_checked, Mint, TokenAccount, TokenInterface, TransferChecked,
};
use openfiat_programs_shared::ProposalCategory;

use crate::shared_logic::{quorum_bps_for, threshold_bps_for};
use crate::{constants::*, error::ErrorCode, events::ProposalCreated, state::*};

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

    #[account(mint::token_program = token_program)]
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

    /// What this proposal will be entitled to do if it passes, fixed
    /// here and never writable again.
    ///
    /// Attached at creation rather than by a later "attach action"
    /// instruction, and that timing is the security property, not a
    /// convenience. An action that could be added or changed after
    /// voting opened would let a proposer collect votes on a harmless
    /// proposal and then point the passed vote at a wallet nobody
    /// agreed to exclude. `init` here means the action is visible to
    /// the first voter and identical to the last.
    ///
    /// Every proposal carries one, including the purely informational
    /// ones that carry `GovernanceAction::None`. Making it optional
    /// would save those proposals a few thousand lamports of rent and
    /// cost every reader — on-chain and off — a branch on whether the
    /// account exists, which is the sort of branch that eventually gets
    /// one path wrong.
    #[account(
        init,
        payer = proposer,
        space = 8 + ProposalAction::INIT_SPACE,
        seeds = [PROPOSAL_ACTION_SEED, proposal.key().as_ref()],
        bump
    )]
    pub proposal_action: Account<'info, ProposalAction>,

    pub token_program: Interface<'info, TokenInterface>,
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
    action: GovernanceAction,
) -> Result<()> {
    require!(
        !openfiat_programs_shared::wallet_is_banned(&ctx.accounts.ban_record),
        ErrorCode::WalletBanned
    );

    require!(voting_period_secs > 0, ErrorCode::InvalidVoteLock);

    // A ban-list action fixes its own category, so the bar it has to
    // clear is not the proposer's to choose. `Standards` is where
    // OFS-7100 §12 lives — it is protocol policy, not a numeric
    // parameter (`Parameter`) and not a disbursement (`Treasury`) — and
    // it settles the thresholds for both directions at once: listing and
    // delisting face the identical quorum and majority. §12.2 requires
    // that symmetry, and §15 depends on it, since a readmission harder
    // to win than the exclusion that preceded it makes every false
    // positive permanent in practice.
    match action {
        GovernanceAction::ListWallet { .. } | GovernanceAction::DelistWallet { .. } => {
            require!(
                category == ProposalCategory::Standards,
                ErrorCode::WrongCategoryForBanAction
            );
        }
        GovernanceAction::None => {}
    }

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
    // All zeroes is the "no off-chain counterpart claimed" sentinel;
    // `link_offchain_proposal` is the only thing that ever changes it,
    // and only once. Set explicitly rather than relying on `init`'s
    // zeroing, so the sentinel is a decision this function made.
    proposal.offchain_id_hash = [0u8; 32];

    let proposal_key = proposal.key();
    let voting_ends_at = proposal.voting_ends_at;

    let proposal_action = &mut ctx.accounts.proposal_action;
    proposal_action.proposal = proposal_key;
    proposal_action.action = action;
    proposal_action.bump = ctx.bumps.proposal_action;

    emit!(ProposalCreated {
        proposal: proposal_key,
        proposal_id: id,
        proposer: ctx.accounts.proposer.key(),
        category,
        title_hash,
        summary_hash,
        action,
        proposal_action: proposal_action.key(),
        stake_deposit: deposit_amount,
        quorum_snapshot,
        threshold_snapshot: threshold_bps,
        voting_ends_at,
        timestamp: now,
    });
    Ok(())
}
