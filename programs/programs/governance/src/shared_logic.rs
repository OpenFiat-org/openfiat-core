//! Category-to-threshold lookup — shared by `create_proposal` (which
//! snapshots these onto the `Proposal`) so the mapping lives in one
//! place — plus the single definition of what makes a passed proposal
//! executable.

use anchor_lang::prelude::*;
use openfiat_programs_shared::ProposalCategory;

use crate::error::ErrorCode;
use crate::state::{EmergencyAuthority, GovernanceConfig, Proposal, ProposalState};

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

/// Whether AllenHark's first-year exception is still open (OFS-4100
/// §5.1).
///
/// Half-open by design: the window is `[initialized_at, expires_at)`, so
/// the power is already gone at the instant `now == expires_at`. "The
/// first year after initialization, and no longer" leaves no second in
/// which the deadline has arrived and the power still works, and an
/// inclusive comparison would invent one.
///
/// A free function over a plain timestamp rather than a method taking the
/// `Clock`, so the boundary can be tested at, before and after expiry.
/// Nothing else can: `solana-test-validator`'s clock tracks wall time and
/// cannot be advanced a year, so every on-validator test necessarily runs
/// inside the window. The instructions call this; these unit tests are
/// what prove what it does on the far side of the deadline.
pub fn emergency_powers_available(authority: &EmergencyAuthority, now: i64) -> bool {
    now < authority.expires_at
}

/// [`emergency_powers_available`] as a guard.
pub fn require_within_emergency_window(authority: &EmergencyAuthority, now: i64) -> Result<()> {
    require!(
        emergency_powers_available(authority, now),
        ErrorCode::EmergencyPowersExpired
    );
    Ok(())
}

/// The whole of the sunset's effect on `update_governance_config`, as one
/// testable function (OFS-4100 §5.1).
///
/// Extracted rather than written inline in the instruction so the *rule*
/// can be tested past the deadline, not just the clock comparison it
/// rests on. A `solana-test-validator` cannot be moved a year forward, so
/// an on-validator test can only ever exercise the inside of the window;
/// leaving the rule inline would have meant the branch that actually
/// takes the power away was never executed by anything.
///
/// Only a *change* is refused, never the field's presence. `vote_lock_secs`
/// stays in the params struct after the sunset and a caller may keep
/// echoing it back unchanged, so every other parameter remains
/// governance-configurable for ever — the exception lapsing must not
/// freeze the whole config.
pub fn require_vote_lock_change_allowed(
    authority: &EmergencyAuthority,
    now: i64,
    current_vote_lock_secs: i64,
    requested_vote_lock_secs: i64,
) -> Result<()> {
    if emergency_powers_available(authority, now) {
        return Ok(());
    }
    require!(
        requested_vote_lock_secs == current_vote_lock_secs,
        ErrorCode::EmergencyPowersExpired
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::FIRST_YEAR_SECS;

    /// `expires_at` exactly as `initialize_emergency_authority` computes
    /// it, so these tests exercise the real span rather than a convenient
    /// one.
    fn authority_initialized_at(initialized_at: i64) -> EmergencyAuthority {
        EmergencyAuthority {
            primary_holder: crate::constants::ALLENHARK_PRIMARY_HOLDER,
            secondary_holder: crate::constants::ALLENHARK_SECONDARY_HOLDER,
            initialized_at,
            expires_at: initialized_at + FIRST_YEAR_SECS,
            bump: 255,
        }
    }

    #[test]
    fn the_exception_is_open_for_the_whole_first_year() {
        let authority = authority_initialized_at(1_000);
        assert!(emergency_powers_available(&authority, 1_000));
        assert!(emergency_powers_available(
            &authority,
            1_000 + FIRST_YEAR_SECS - 1
        ));
    }

    #[test]
    fn the_exception_is_closed_at_the_very_instant_it_expires() {
        // The boundary case the half-open window exists for. An
        // inclusive check here would leave one second in which the
        // deadline has arrived and the power still works.
        let authority = authority_initialized_at(1_000);
        assert!(!emergency_powers_available(
            &authority,
            1_000 + FIRST_YEAR_SECS
        ));
        assert!(
            require_within_emergency_window(&authority, 1_000 + FIRST_YEAR_SECS).is_err(),
            "the guard must refuse at expiry, not merely after it"
        );
    }

    #[test]
    fn the_exception_never_reopens_however_long_you_wait() {
        // There is no periodic renewal and no second window: this is the
        // property that makes it a sunset rather than a cooldown.
        let authority = authority_initialized_at(1_000);
        for years_later in 1..=10 {
            let now = 1_000 + FIRST_YEAR_SECS * years_later;
            assert!(
                !emergency_powers_available(&authority, now),
                "the exception must stay closed {years_later} year(s) past expiry"
            );
        }
        assert!(!emergency_powers_available(&authority, i64::MAX));
    }

    #[test]
    fn the_window_is_exactly_one_year_wide_wherever_the_clock_starts() {
        // Nothing about the initializing transaction can change the span
        // — the only thing a caller influences is when it starts.
        for start in [0, 1, 1_700_000_000, i64::MAX - FIRST_YEAR_SECS] {
            let authority = authority_initialized_at(start);
            assert_eq!(
                authority.expires_at - authority.initialized_at,
                FIRST_YEAR_SECS
            );
        }
    }

    #[test]
    fn the_delay_power_still_works_inside_the_window() {
        // The control. A sunset that closes a power which never worked
        // proves nothing, so the "before" case is asserted as
        // deliberately as the "after".
        let authority = authority_initialized_at(1_000);
        assert!(
            require_vote_lock_change_allowed(&authority, 1_000, 604_800, 30 * 24 * 60 * 60).is_ok(),
            "vote_lock_secs must be writable while the exception is open"
        );
    }

    #[test]
    fn the_delay_power_is_gone_once_the_exception_lapses() {
        // The point of #121, executed rather than described. This is the
        // branch a validator test can never reach, because a test
        // validator's clock cannot be moved a year forward.
        let authority = authority_initialized_at(1_000);
        let after = 1_000 + FIRST_YEAR_SECS;
        assert!(
            require_vote_lock_change_allowed(&authority, after, 604_800, 604_801).is_err(),
            "a one-second increase is still a change, and changes are over"
        );
        assert!(
            require_vote_lock_change_allowed(&authority, after, 604_800, 1).is_err(),
            "shortening the delay is a change too — the field is frozen, not capped"
        );
    }

    #[test]
    fn a_lapsed_exception_freezes_only_the_delay_and_not_the_whole_config() {
        // `vote_lock_secs` stays in the params struct after the sunset, so
        // an admin correcting quorum or a fee has to send *something* in
        // that field. Echoing the stored value back must keep working, or
        // the exception lapsing would freeze every parameter with it.
        let authority = authority_initialized_at(1_000);
        let after = 1_000 + FIRST_YEAR_SECS * 3;
        assert!(
            require_vote_lock_change_allowed(&authority, after, 604_800, 604_800).is_ok(),
            "an unchanged value must remain acceptable for ever"
        );
    }

    /// The addresses OFS-4100 §5.1 signed off, transcribed from the
    /// specification rather than from `constants.rs`. A typo in either
    /// constant would otherwise be invisible: nothing else in this
    /// program compares them against anything.
    #[test]
    fn the_recorded_holders_are_the_two_keys_the_specification_names() {
        assert_eq!(
            crate::constants::ALLENHARK_PRIMARY_HOLDER.to_string(),
            "ALLENLMtV1zEAHT3xpVryqcbdPCB8c9JhM1Jdbe5XHg5"
        );
        assert_eq!(
            crate::constants::ALLENHARK_SECONDARY_HOLDER.to_string(),
            "A11ENCKCBxZxEbXQmqs6mTmJkP8gjcA7xqfLD5BxfRpp"
        );
        assert_ne!(
            crate::constants::ALLENHARK_PRIMARY_HOLDER,
            crate::constants::ALLENHARK_SECONDARY_HOLDER,
            "two holders that are the same key are one holder"
        );
    }
}
