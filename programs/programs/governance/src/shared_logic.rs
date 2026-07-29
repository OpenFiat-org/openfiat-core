//! Category-to-threshold lookup — shared by `create_proposal` (which
//! snapshots these onto the `Proposal`) so the mapping lives in one
//! place — plus the single definition of what makes a passed proposal
//! executable.

use anchor_lang::prelude::*;
use openfiat_programs_shared::ProposalCategory;

use crate::error::ErrorCode;
use crate::state::{GovernanceConfig, Proposal, ProposalState};

/// OFS-4100 §5: standard categories use `quorum_bps`; Protocol-Upgrade
/// and Constitutional use the higher `quorum_upgrade_bps`.
pub fn quorum_bps_for(config: &GovernanceConfig, category: ProposalCategory) -> u16 {
    match category {
        ProposalCategory::ProtocolUpgrade | ProposalCategory::Constitutional => {
            config.quorum_upgrade_bps
        }
        _ => config.quorum_bps,
    }
}

/// OFS-4100 §5: simple majority for Informational/Standards/Parameter,
/// a higher bar for Treasury, and the highest for Protocol-Upgrade/
/// Constitutional.
pub fn threshold_bps_for(config: &GovernanceConfig, category: ProposalCategory) -> u16 {
    match category {
        ProposalCategory::Treasury => config.threshold_treasury_bps,
        ProposalCategory::ProtocolUpgrade | ProposalCategory::Constitutional => {
            config.threshold_upgrade_bps
        }
        _ => config.threshold_simple_bps,
    }
}

/// The complete definition of "this passed proposal may now act", in one
/// place so no execution instruction can implement a subtly weaker
/// version of it.
///
/// Four conditions, each guarding a distinct failure:
///
/// * **`Accepted`** — the vote went the proposer's way. `Rejected` and
///   `Voting` both authorize nothing.
/// * **`quorum_met`** — checked separately from `Accepted` even though
///   `tally_and_finalize` only reaches `Accepted` through quorum today.
///   The two are distinct fields recording distinct facts, and a future
///   change to the tally that decoupled them must not silently make a
///   two-voter proposal able to exclude a wallet from the protocol.
/// * **`!executed`** — set by the caller in the same instruction, so an
///   authorization is spent exactly once. Without it a single passed
///   ban proposal could be replayed against its target forever, and
///   more importantly a *delisting* could be undone by re-running the
///   listing that preceded it.
/// * **the timelock** — `vote_lock_secs` after voting closed. A tally is
///   permissionless and can land in the same slot voting ends, so
///   without a delay the first observer of an accepted proposal could
///   execute it before anyone else had seen it pass. The delay is what
///   gives an erroneously-listed wallet, or a governance attack's
///   victims, a window to react while the decision is public but not
///   yet enforced.
///
/// Deliberately *not* a condition: who submits the transaction. See
/// `list_wallet`'s doc comment.
pub fn require_executable(proposal: &Proposal, config: &GovernanceConfig, now: i64) -> Result<()> {
    require!(
        proposal.state == ProposalState::Accepted,
        ErrorCode::ProposalNotAccepted
    );
    require!(proposal.quorum_met, ErrorCode::QuorumNotMet);
    require!(!proposal.executed, ErrorCode::AlreadyExecuted);

    let executable_at = proposal
        .voting_ends_at
        .checked_add(config.vote_lock_secs)
        .ok_or(ErrorCode::Overflow)?;
    require!(now >= executable_at, ErrorCode::ExecutionTimelockActive);
    Ok(())
}

/// `vote_lock_secs` must be positive and no longer than
/// [`crate::constants::MAX_VOTE_LOCK_SECS`].
///
/// The upper bound is the half that is about security rather than
/// sanity. `vote_lock_secs` is the delay before an accepted proposal may
/// act, and `admin` can still write it. Unbounded, one key could set it
/// beyond any horizon and leave every accepted proposal permanently
/// unexecutable — including a proposal to delist a wallet, which
/// reinstates in a different shape the exact power the ban list was
/// re-gated to remove.
pub fn require_valid_vote_lock(vote_lock_secs: i64) -> Result<()> {
    require!(vote_lock_secs > 0, ErrorCode::InvalidVoteLock);
    require!(
        vote_lock_secs <= crate::constants::MAX_VOTE_LOCK_SECS,
        ErrorCode::VoteLockTooLong
    );
    Ok(())
}
