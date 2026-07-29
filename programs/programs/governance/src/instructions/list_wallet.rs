use anchor_lang::prelude::*;

use crate::shared_logic::require_executable;
use crate::{constants::*, error::ErrorCode, events::WalletListed, state::*};

/// Adds a wallet to the protocol-wide ban list (OFS-7100 §12), executing
/// a proposal that has already passed.
///
/// Creating the `BanRecord` PDA *is* the ban. Every gated instruction in
/// `escrow`, `staking`, `presale` and this program derives the same
/// address from its own signer's key and refuses to run if it is
/// occupied, so this one account creation closes deposit access
/// protocol-wide in a single transaction. Nothing else has to be
/// notified and no application can opt out — which is precisely what
/// §12 asks for, and precisely why the authority question matters.
///
/// # Authority: a passed proposal, and nothing else
///
/// This instruction has no privileged signer. It does not read
/// `GovernanceConfig.admin`, and no constraint anywhere in it names a
/// particular key. What it requires instead is a `Proposal` that the
/// protocol accepted — see [`require_executable`] for the four
/// conditions — carrying a [`ProposalAction`] that names *this* wallet,
/// with this reason and this evidence.
///
/// The previous version checked `GovernanceConfig.admin`, which meant
/// one key could deny any wallet deposit access to the entire protocol
/// and the same key could restore it. That was not a shortcut taken
/// here: no accepted proposal could cause any state change anywhere, so
/// gating on a vote would have gated on machinery that could not fire.
/// The machinery is `ProposalAction` plus `require_executable`, and it
/// is what this instruction now runs on.
///
/// # Why anyone may submit it
///
/// `submitter` signs and funds the rent, and that is all it does. Making
/// execution permissionless is not a convenience: if a named party had
/// to submit, that party could decline, and declining to execute a
/// passed *delisting* is indistinguishable from an unappealable ban. The
/// vote decides; whoever is willing to pay the transaction fee carries
/// it out.
///
/// # The reason and evidence come from the proposal, not the caller
///
/// `reason` and `evidence_hash` are read out of the `ProposalAction`,
/// never from instruction arguments. `wallet` *is* an argument, because
/// the `BanRecord` PDA is seeded on it, but it is checked against the
/// action before anything is written — a proposal to ban wallet A
/// cannot be redeemed against wallet B. Were the reason a caller
/// argument, the same passed proposal would let its submitter record
/// grounds the voters never agreed to, against the one artefact §15
/// gives an erroneously-listed wallet to contest.
///
/// # Open decision: no emergency fast path  `[UNRESOLVED]`
///
/// A wallet actively draining stolen funds is excluded on the same
/// timetable as everything else: voting period, then `vote_lock_secs`.
/// Whether that is fast enough has been asked and not yet answered, so
/// nothing faster is implemented here — an emergency path invented
/// without an answer is a second authority nobody sized.
///
/// If one is ever added it must be a timelocked multisig with mandatory
/// governance ratification, never a single key, and the readmission path
/// must be at least as fast as the exclusion path it shortcuts.
/// Otherwise the emergency path becomes the ordinary path, and this
/// instruction's guarantee is worth exactly what the multisig's weakest
/// member is.
#[derive(Accounts)]
#[instruction(wallet: Pubkey)]
pub struct ListWallet<'info> {
    /// Any signer. Pays the `BanRecord`'s rent and the transaction fee;
    /// confers no authority. Deliberately unconstrained — no line in
    /// this struct or its handler compares this key to anything.
    #[account(mut)]
    pub submitter: Signer<'info>,

    /// Read for `vote_lock_secs`, the execution timelock. `admin` is
    /// not consulted.
    #[account(seeds = [GOVERNANCE_CONFIG_SEED], bump = governance_config.bump)]
    pub governance_config: Box<Account<'info, GovernanceConfig>>,

    /// The passed proposal being executed. `mut` because executing it
    /// spends it: `executed` is set in this same instruction.
    #[account(
        mut,
        seeds = [PROPOSAL_SEED, &proposal.id.to_le_bytes()],
        bump = proposal.bump,
        constraint = proposal.category == openfiat_programs_shared::ProposalCategory::Standards @ ErrorCode::WrongCategoryForBanAction,
    )]
    pub proposal: Box<Account<'info, Proposal>>,

    /// What that proposal authorizes. Seeded on the proposal's own
    /// address, so it cannot be paired with a different vote.
    #[account(
        seeds = [PROPOSAL_ACTION_SEED, proposal.key().as_ref()],
        bump = proposal_action.bump,
    )]
    pub proposal_action: Box<Account<'info, ProposalAction>>,

    /// `init` rather than `init_if_needed`: re-listing an
    /// already-listed wallet is a mistake worth surfacing, and letting
    /// it succeed would silently overwrite the original `listed_at` and
    /// evidence hash — the two fields an erroneously-listed wallet would
    /// use to contest the listing under §15.
    #[account(
        init,
        payer = submitter,
        space = 8 + BanRecord::INIT_SPACE,
        seeds = [BAN_SEED, wallet.as_ref()],
        bump,
    )]
    pub ban_record: Box<Account<'info, BanRecord>>,

    pub system_program: Program<'info, System>,
}

pub fn handle_list_wallet(ctx: Context<ListWallet>, wallet: Pubkey) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    require_executable(&ctx.accounts.proposal, &ctx.accounts.governance_config, now)?;

    // The binding. A proposal authorizes one action against one wallet;
    // anything else — a delisting proposal, a `None` proposal, or a
    // listing proposal naming somebody else — authorizes nothing here.
    let (reason, evidence_hash) = match ctx.accounts.proposal_action.action {
        GovernanceAction::ListWallet {
            wallet: target,
            reason,
            evidence_hash,
        } if target == wallet => (reason, evidence_hash),
        _ => return err!(ErrorCode::ProposalActionMismatch),
    };

    let authorizing_proposal = ctx.accounts.proposal.key();
    let proposal_id = ctx.accounts.proposal.id;

    let ban_record = &mut ctx.accounts.ban_record;
    ban_record.wallet = wallet;
    ban_record.reason = reason;
    ban_record.evidence_hash = evidence_hash;
    ban_record.listed_at = now;
    ban_record.authorizing_proposal = authorizing_proposal;
    ban_record.bump = ctx.bumps.ban_record;

    // Spent in the same instruction that acts on it, so the ban cannot
    // be replayed and — the case that actually matters — a later
    // delisting cannot be undone by re-running the proposal that
    // originally listed the wallet.
    ctx.accounts.proposal.executed = true;

    emit!(WalletListed {
        wallet,
        reason,
        evidence_hash,
        authorizing_proposal,
        proposal_id,
        timestamp: now,
    });
    Ok(())
}
