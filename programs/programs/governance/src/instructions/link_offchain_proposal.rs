use anchor_lang::prelude::*;

use crate::{constants::*, error::ErrorCode, events::OffchainProposalLinked, state::*};

/// Records which off-chain proposal this on-chain one is the chain-side
/// half of, so a client can put the two side by side and see whether they
/// agree.
///
/// # Why this is a separate instruction rather than an argument to `create_proposal`
///
/// Purely so the link can be added without changing `create_proposal`'s
/// argument list. That instruction is called from several test suites and
/// from both SDKs; an extra positional argument would break every one of
/// them for a field most proposals do not need. The cost is that the link
/// is attached in a second transaction, which is what the rest of this
/// comment is about.
///
/// # Why attaching it after voting opens is still safe
///
/// [`ProposalAction`] is fixed at creation because it is what a passed
/// vote is *authorized to do* — a voter must see it before voting. This
/// is not that. It changes no power, gates no execution, and moves no
/// funds; it is an identity claim, and the whole design assumes it is
/// only half of one.
///
/// A link exists when the two sides agree. This side is written here, by
/// the proposer. The other side — the off-chain proposal naming this
/// proposal's `u64` id — lives inside a `ProposalCreate` gossip event
/// that its author signed when the off-chain proposal was created, and
/// which cannot be amended afterwards. So the proposer cannot point a
/// live vote at an off-chain proposal that did not already, at its own
/// creation, name this one. The worst available move is to claim an
/// off-chain proposal that never claimed back, which the reader detects
/// as a one-sided claim and reports as exactly that.
///
/// Two further limits keep even that narrow. It may only be set while the
/// proposal is still in `Voting`, so a link cannot be bolted onto a
/// decision that has already been reached; and it may only be set once,
/// enforced against the all-zero sentinel below, so the claim a voter saw
/// is the claim a reader gets.
#[derive(Accounts)]
pub struct LinkOffchainProposal<'info> {
    /// The proposal's own proposer. Every *execution* path in this
    /// program is deliberately permissionless — the authority is the vote
    /// — but this is not an execution path, and a permissionless version
    /// would let any passer-by attach a claim the proposer never made and
    /// then spend the one-shot write. `Proposal::proposer` is the only
    /// party with standing over its own identity fields.
    pub proposer: Signer<'info>,

    #[account(
        mut,
        seeds = [PROPOSAL_SEED, &proposal.id.to_le_bytes()],
        bump = proposal.bump,
        constraint = proposal.proposer == proposer.key() @ ErrorCode::NotTheProposer,
        constraint = proposal.state == ProposalState::Voting @ ErrorCode::NotInVotingState,
    )]
    pub proposal: Account<'info, Proposal>,
}

pub fn handle_link_offchain_proposal(
    ctx: Context<LinkOffchainProposal>,
    offchain_id_hash: [u8; 32],
) -> Result<()> {
    // All zeroes is the "no off-chain counterpart" sentinel, so accepting
    // it as a link would both write nothing and consume the one-shot
    // write, leaving the proposal permanently unlinkable while looking
    // unlinked. Rejecting it keeps "unset" and "set to nothing"
    // indistinguishable, which is the only way a reader can trust the
    // sentinel.
    require!(
        offchain_id_hash != [0u8; 32],
        ErrorCode::EmptyOffchainIdHash
    );

    let proposal = &mut ctx.accounts.proposal;
    require!(
        proposal.offchain_id_hash == [0u8; 32],
        ErrorCode::OffchainLinkAlreadySet
    );
    proposal.offchain_id_hash = offchain_id_hash;

    emit!(OffchainProposalLinked {
        proposal: proposal.key(),
        proposal_id: proposal.id,
        offchain_id_hash,
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}
