//! How long a node keeps the content it holds.
//!
//! # Not every node should store everything forever
//!
//! The first version of content pinning had no notion of time: a node
//! kept every block it ever saw. That is wrong in the direction that
//! matters, because it makes running a node an ever-growing commitment
//! and quietly turns every operator into an archivist. Most nodes should
//! carry a recent window; a few should carry everything, deliberately and
//! because their operator chose to.
//!
//! So a node declares a [`Retention`] and evicts past it.
//!
//! # The protocol minimum is what makes a challenge fair
//!
//! Eviction and the reward challenge pull against each other. A node that
//! correctly drops old content will fail a challenge about it, and paying
//! it less for doing exactly what it was configured to do would be
//! nonsense — but letting each node declare its own window and be
//! challenged only within it is worse, because the window is
//! self-reported and a node claiming "I keep one hour" would store
//! nothing and still earn.
//!
//! [`MINIMUM_DAYS`] resolves it. It is a floor every node owes the
//! network regardless of configuration, and challenges are only ever
//! drawn from content inside it (see [`Retention::challenge_window`]).
//! A rolling node that honours the floor passes every challenge it can be
//! asked; an archival node keeps more and is asked no more. Neither is
//! penalised for the other's choice, and no node can shrink its
//! obligation by declaring a smaller window.
//!
//! # This is a window on content, and only on content
//!
//! `--retention` reads like a whole-node setting and is not one. A node
//! keeps several other things — a gossip event log, snapshot
//! announcements, records with their own `expires_at`, and the
//! marketplace records, which are kept for good — and each is bounded by
//! its own rule rather than by this type. `Retention` is passed to
//! exactly the two places that hold bytes on a node's behalf: the
//! eviction sweep over [`crate::HeldContent`], and pinning into an
//! operator's own IPFS daemon.
//!
//! The whole list, and why each entry is bounded the way it is, lives in
//! one place: `openfiat_rpc::actor::poll_expired_records`. Keeping it
//! there rather than here is deliberate — this crate knows about content
//! and the node crate is the only one that sees everything a node holds.

use openfiat_types::Timestamp;

/// The window every node owes the network, whatever it is configured to
/// keep.
///
/// Thirty days, chosen against the dispute lifecycle rather than picked
/// for roundness: evidence matters until a dispute can no longer be
/// opened or is resolved, and a trade's escrow, dispute window and
/// arbitration rounds all close well inside a month. Content older than
/// that is history — worth keeping, but not something every node must be
/// holding for the network to function.
///
/// `[PROPOSED — NEEDS SIGN-OFF]`.
pub const MINIMUM_DAYS: u64 = 30;

const MILLIS_PER_DAY: u64 = 24 * 60 * 60 * 1_000;

/// What a node keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retention {
    /// Keep content for `days`, then evict. The default: a node should be
    /// a bounded commitment unless its operator says otherwise.
    Rolling { days: u64 },
    /// Keep everything, forever. An explicit choice by an operator who
    /// intends to run an archive and has the disk for it.
    Archival,
}

impl Default for Retention {
    fn default() -> Self {
        Self::Rolling { days: MINIMUM_DAYS }
    }
}

impl Retention {
    /// Parses `--retention`: `archival`, or a number of days.
    ///
    /// A window shorter than [`MINIMUM_DAYS`] is rejected rather than
    /// silently raised. An operator who asked for seven days and got
    /// thirty would be running something other than what they configured,
    /// and would find out from a disk graph rather than from us.
    pub fn parse(input: &str) -> Result<Self, String> {
        if input.eq_ignore_ascii_case("archival") {
            return Ok(Self::Archival);
        }
        let days: u64 = input
            .parse()
            .map_err(|_| format!("expected `archival` or a number of days, got {input:?}"))?;
        if days < MINIMUM_DAYS {
            return Err(format!(
                "{days} days is below the {MINIMUM_DAYS}-day minimum every node owes the network; \
                 use {MINIMUM_DAYS} or more, or `archival`"
            ));
        }
        Ok(Self::Rolling { days })
    }

    /// Whether content created at `created_at` is still within this
    /// node's own window as of `now`.
    pub fn keeps(&self, created_at: Timestamp, now: Timestamp) -> bool {
        match self {
            Self::Archival => true,
            Self::Rolling { days } => within(created_at, now, *days),
        }
    }

    /// Whether content created at `created_at` may be used to challenge
    /// *any* node, as of `now`.
    ///
    /// Deliberately not a function of `self`. A challenger asks about
    /// content inside the protocol floor, not inside its own retention —
    /// an archival node must not be able to challenge a rolling one about
    /// year-old content, and a rolling node must not be able to shrink
    /// what it can be asked by configuring a smaller window.
    pub fn challenge_window(created_at: Timestamp, now: Timestamp) -> bool {
        within(created_at, now, MINIMUM_DAYS)
    }

    /// For logging: what this node is committed to holding.
    pub fn describe(&self) -> String {
        match self {
            Self::Archival => "archival (everything)".to_string(),
            Self::Rolling { days } => format!("rolling {days}d"),
        }
    }
}

fn within(created_at: Timestamp, now: Timestamp, days: u64) -> bool {
    let age = now.as_millis().saturating_sub(created_at.as_millis());
    age <= days.saturating_mul(MILLIS_PER_DAY)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `now()` is far enough from the epoch that "ten years ago" is still
    /// a real instant rather than an underflow in the test helper itself.
    fn now() -> Timestamp {
        Timestamp::from_millis(20_000 * MILLIS_PER_DAY)
    }

    fn days_ago(n: u64) -> Timestamp {
        Timestamp::from_millis(now().as_millis() - n * MILLIS_PER_DAY)
    }

    #[test]
    fn a_rolling_node_keeps_its_window_and_drops_what_is_past_it() {
        let retention = Retention::Rolling { days: 30 };
        assert!(retention.keeps(days_ago(29), now()));
        assert!(retention.keeps(days_ago(30), now()));
        assert!(!retention.keeps(days_ago(31), now()));
    }

    #[test]
    fn an_archival_node_keeps_everything() {
        assert!(Retention::Archival.keeps(days_ago(3_650), now()));
    }

    #[test]
    fn the_default_is_bounded_rather_than_forever() {
        // Running a node must not be an open-ended storage commitment
        // that an operator discovers only when the disk fills.
        assert_eq!(
            Retention::default(),
            Retention::Rolling { days: MINIMUM_DAYS }
        );
    }

    #[test]
    fn a_window_below_the_minimum_is_refused_not_quietly_raised() {
        let err = Retention::parse("7").unwrap_err();
        assert!(err.contains("below the 30-day minimum"), "{err}");
        assert!(Retention::parse("30").is_ok());
        assert!(Retention::parse("365").is_ok());
    }

    #[test]
    fn archival_parses_case_insensitively_and_nonsense_does_not() {
        assert_eq!(Retention::parse("archival"), Ok(Retention::Archival));
        assert_eq!(Retention::parse("ARCHIVAL"), Ok(Retention::Archival));
        assert!(Retention::parse("forever").is_err());
        assert!(Retention::parse("").is_err());
        assert!(Retention::parse("-1").is_err());
    }

    #[test]
    fn the_challenge_window_ignores_the_challengers_own_retention() {
        // The property that keeps eviction and rewards from contradicting
        // each other: an archival node cannot ask a rolling node about
        // content the rolling node was configured to drop.
        assert!(Retention::challenge_window(days_ago(29), now()));
        assert!(!Retention::challenge_window(days_ago(31), now()));
    }

    #[test]
    fn a_node_cannot_shrink_what_it_can_be_asked_by_configuring_less() {
        // `challenge_window` is an associated function precisely so that
        // no `self` — and therefore no operator setting — can narrow it.
        let recent = days_ago(10);
        assert!(Retention::challenge_window(recent, now()));
        // Whatever this node keeps, it can still be asked about `recent`.
        for retention in [Retention::Archival, Retention::Rolling { days: 30 }] {
            assert!(retention.keeps(recent, now()));
        }
    }
}
