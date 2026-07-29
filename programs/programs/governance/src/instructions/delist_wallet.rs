use anchor_lang::prelude::*;

use crate::shared_logic::require_executable;
use crate::{constants::*, error::ErrorCode, events::WalletDelisted, state::*};

/// Removes a wallet from the ban list (OFS-7100 §12.2), restoring its
/// deposit access protocol-wide, by executing a proposal that has
/// already passed.
///
/// Closing the `BanRecord` returns its address to the "non-existent
/// account" state every gate treats as unbanned, so access is restored
/// everywhere by the same single account operation that removed it — no
/// per-program cleanup, and no window in which some programs still
/// refuse the wallet.
///
/// §12.2 makes this mandatory rather than optional, and §15 explains the
/// cost of getting it wrong: once rejection is protocol-wide, "an
/// erroneous listing now costs a wallet all protocol access rather than
/// one application's caution". A ban list without a working delist path
/// would turn every false positive into a permanent one.
///
/// # Authority: identical to `list_wallet`, by construction
///
/// Same guard ([`require_executable`]), same category, same
/// [`ProposalAction`] binding, same absence of any privileged signer.
/// That is not tidiness — an authority able to exclude but not readmit
/// is the failure §12.2 names, and the surest way to avoid drifting into
/// it is for both directions to run through one shared definition of
/// what a passed proposal is. Anything that later narrows execution has
/// to narrow readmission at the same moment, or fail to compile past
/// one call site.
///
/// Rent from the closed record goes to whoever submits the delisting.
/// The listing's rent was paid by a submitter with no more standing than
/// this one, so there is no original payer with a claim on it; leaving
/// it as a small bounty on executing a passed readmission is the more
/// useful place for it, and §15 wants readmission to be the cheap,
/// attractive path. Nothing is taken from the listed wallet in either
/// direction — it never funded the record.
#[derive(Accounts)]
#[instruction(wallet: Pubkey)]
pub struct DelistWallet<'info> {
    /// Any signer. Receives the closed record's rent and pays the
    /// transaction fee; confers no authority, and no line here compares
    /// this key to anything.
    #[account(mut)]
    pub submitter: Signer<'info>,

    /// Read for `vote_lock_secs`, the execution timelock. `admin` is
    /// not consulted.
    #[account(seeds = [GOVERNANCE_CONFIG_SEED], bump = governance_config.bump)]
    pub governance_config: Box<Account<'info, GovernanceConfig>>,

    #[account(
        mut,
        seeds = [PROPOSAL_SEED, &proposal.id.to_le_bytes()],
        bump = proposal.bump,
        constraint = proposal.category == openfiat_programs_shared::ProposalCategory::Standards @ ErrorCode::WrongCategoryForBanAction,
    )]
    pub proposal: Box<Account<'info, Proposal>>,

    #[account(
        seeds = [PROPOSAL_ACTION_SEED, proposal.key().as_ref()],
        bump = proposal_action.bump,
    )]
    pub proposal_action: Box<Account<'info, ProposalAction>>,

    /// Seeded on the `wallet` argument, so the address is derived rather
    /// than trusted; `close` then zeroes the account and hands back the
    /// rent, and the zeroed address is what the gates read as unbanned.
    #[account(
        mut,
        close = submitter,
        seeds = [BAN_SEED, wallet.as_ref()],
        bump = ban_record.bump,
    )]
    pub ban_record: Box<Account<'info, BanRecord>>,
}

pub fn handle_delist_wallet(ctx: Context<DelistWallet>, wallet: Pubkey) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    require_executable(&ctx.accounts.proposal, &ctx.accounts.governance_config, now)?;

    match ctx.accounts.proposal_action.action {
        GovernanceAction::DelistWallet { wallet: target } if target == wallet => {}
        _ => return err!(ErrorCode::ProposalActionMismatch),
    }

    let authorizing_proposal = ctx.accounts.proposal.key();
    let proposal_id = ctx.accounts.proposal.id;
    ctx.accounts.proposal.executed = true;

    emit!(WalletDelisted {
        wallet,
        authorizing_proposal,
        proposal_id,
        listed_at: ctx.accounts.ban_record.listed_at,
        listed_by_proposal: ctx.accounts.ban_record.authorizing_proposal,
        timestamp: now,
    });
    Ok(())
}
