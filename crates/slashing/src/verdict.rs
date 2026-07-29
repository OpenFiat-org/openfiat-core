//! The verdict: who revealed against consensus on a resolved case.

use std::collections::BTreeSet;

/// Recorded on every `SlashApplied` event so the reason survives on-chain
/// rather than living only in this crate.
///
/// `1` is OFS-4100 §4's first enumerated trigger slot. The value is
/// deliberately small and stable; renumbering it would orphan every event
/// already emitted. `[PROPOSED — NEEDS SIGN-OFF]` — §4 enumerates the
/// triggers but assigns them no codes.
pub const MISCONDUCT_OUTSIDE_CONSENSUS: u16 = 1;

/// One arbitrator's seat on a resolved case, as recorded on-chain.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Seat {
    /// The arbitrator's wallet — `DisputeCase.arbitrators[i]`, and the
    /// `owner` half of the `StakeAccount` PDA `slash` will target.
    pub arbitrator: [u8; 32],
    /// `DisputeCase.revealed_outcomes[i]`: `None` when that seat never
    /// revealed. Encoded as the outcome's on-chain discriminant so this
    /// crate does not need to depend on the Anchor program's enum.
    pub revealed_outcome: Option<u8>,
    /// `DisputeCase.weights[i]` — effective stake read at reveal time.
    pub weight: u64,
}

/// A dispute the escrow program has already tallied.
///
/// Mirrors the on-chain `DisputeCase` fields this decision needs, rather
/// than depending on the Anchor account type: the caller decodes, this
/// crate decides. Keeps the money logic testable without a validator.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ResolvedCase {
    pub reservation_id: u64,
    /// Which arbitration round produced [`Self::outcome`]. Part of the
    /// idempotence key: a case that re-opened has distinct rounds, and
    /// each decides at most once.
    pub round: u8,
    /// `DisputeCase.outcome` — `Some` only when a round reached a
    /// stake-weighted verdict. `None` means the case exhausted its rounds
    /// and split the escrow evenly.
    pub outcome: Option<u8>,
    pub seats: Vec<Seat>,
}

/// One arbitrator to slash.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SlashEntry {
    pub arbitrator: [u8; 32],
    /// What they revealed, retained so the submitted instruction can be
    /// audited against the tally without re-fetching the case.
    pub revealed_outcome: u8,
    pub weight: u64,
    pub misconduct_code: u16,
}

/// Everyone to slash for one decided round, and nobody else.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct SlashSchedule {
    pub reservation_id: u64,
    pub round: u8,
    /// The outcome the tally settled on, which the entries dissented from.
    pub outcome: u8,
    pub entries: Vec<SlashEntry>,
}

impl SlashSchedule {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Who must be slashed for `case`.
///
/// Returns `None` when the case did not produce a verdict — an
/// undecided case condemns nobody, because there is no consensus for a
/// vote to have fallen outside of.
///
/// # Who is slashed
///
/// Exactly those seats whose **revealed** outcome differs from the tallied
/// one, per OFS-2400 §328: "arbitrators whose revealed vote falls outside
/// consensus may incur a partial, moderate stake slash".
///
/// # Who is not, and why
///
/// **A seat that never revealed is not slashed.** That is a deliberate
/// reading of the specifications rather than an oversight, and it is the
/// one judgement call in this crate:
///
/// - OFS-2400 §328 conditions the penalty on a *revealed* vote. A silent
///   seat has no revealed vote to compare against consensus, so the rule
///   as written does not reach it.
/// - OFS-4100 §4's trigger 1, "arbitrator misses a case decision
///   deadline", is the clause that *would* reach it — and §4 states
///   plainly that triggers 1 and 3 "depend on future amendments to
///   OFS-2400 and OFS-1600 respectively to define concrete
///   deadlines/thresholds; until those exist, only triggers 2 and 4 are
///   enforceable in v1."
///
/// So slashing non-revealers today would be enforcing a trigger the
/// tokenomics specification says is not yet enforceable. Note the gap is
/// narrower than §4 assumes: `DisputeCase.reveal_deadline` is now a
/// concrete on-chain timestamp, so the missing piece is an OFS-2400
/// amendment sanctioning it, not the data. Failing to reveal is currently
/// punished only indirectly — a silent seat earns no reward and occupies a
/// slot that decides nothing.
///
/// A zero-weight seat that revealed against consensus *is* included. It
/// contributed nothing to the tally, but it consumed a seat, and a slash
/// of zero stake costs it nothing — the entry is what makes the attempt
/// visible in the event log.
pub fn compute(case: &ResolvedCase) -> Option<SlashSchedule> {
    let outcome = case.outcome?;
    let entries = case
        .seats
        .iter()
        .filter_map(|seat| {
            let revealed = seat.revealed_outcome?;
            (revealed != outcome).then_some(SlashEntry {
                arbitrator: seat.arbitrator,
                revealed_outcome: revealed,
                weight: seat.weight,
                misconduct_code: MISCONDUCT_OUTSIDE_CONSENSUS,
            })
        })
        .collect();
    Some(SlashSchedule {
        reservation_id: case.reservation_id,
        round: case.round,
        outcome,
        entries,
    })
}

/// Rounds already slashed, so a case is never punished twice.
///
/// `slash` has no notion of a dispute: it moves `slash_bps` of whatever is
/// staked, every time it is called. Two runs against one verdict is not a
/// duplicate no-op, it is a second confiscation — so the gate has to live
/// here, exactly as `openfiat-rewards`' `PaidEpochs` guards
/// `distribute_reward` for the same reason.
///
/// The key is `(reservation_id, round)` rather than the reservation alone,
/// because a case that failed to decide re-opens and its later round is a
/// genuinely separate verdict. It is not keyed per-arbitrator: a round's
/// schedule is computed and submitted as one unit, and a partial record
/// would be harder to reason about than an all-or-nothing one.
///
/// Callers must persist this. Holding it only in memory means a restart
/// mid-relay can re-slash a round that was already applied, which is the
/// failure this type exists to prevent.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SlashedCases {
    applied: BTreeSet<(u64, u8)>,
}

impl SlashedCases {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_applied(&self, reservation_id: u64, round: u8) -> bool {
        self.applied.contains(&(reservation_id, round))
    }

    /// Marks a round slashed. Returns `false` if it already was, so a
    /// caller can distinguish a fresh mark from a repeat without a
    /// separate read — and should treat `false` as "do not submit".
    pub fn mark_applied(&mut self, reservation_id: u64, round: u8) -> bool {
        self.applied.insert((reservation_id, round))
    }

    pub fn len(&self) -> usize {
        self.applied.len()
    }

    pub fn is_empty(&self) -> bool {
        self.applied.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUYER_WINS: u8 = 0;
    const MERCHANT_WINS: u8 = 1;
    const INVALID: u8 = 3;

    fn seat(id: u8, revealed: Option<u8>, weight: u64) -> Seat {
        Seat {
            arbitrator: [id; 32],
            revealed_outcome: revealed,
            weight,
        }
    }

    fn case(outcome: Option<u8>, seats: Vec<Seat>) -> ResolvedCase {
        ResolvedCase {
            reservation_id: 42,
            round: 0,
            outcome,
            seats,
        }
    }

    #[test]
    fn slashes_only_the_seats_that_revealed_against_consensus() {
        let c = case(
            Some(BUYER_WINS),
            vec![
                seat(1, Some(BUYER_WINS), 10_000),
                seat(2, Some(MERCHANT_WINS), 10_000),
                seat(3, Some(BUYER_WINS), 20_000),
                seat(4, Some(INVALID), 5_000),
            ],
        );
        let schedule = compute(&c).expect("a decided case yields a schedule");
        let slashed: Vec<[u8; 32]> = schedule.entries.iter().map(|e| e.arbitrator).collect();
        assert_eq!(slashed, vec![[2u8; 32], [4u8; 32]]);
        assert!(
            schedule
                .entries
                .iter()
                .all(|e| e.misconduct_code == MISCONDUCT_OUTSIDE_CONSENSUS)
        );
    }

    #[test]
    fn a_seat_that_never_revealed_is_not_slashed() {
        // OFS-4100 §4 trigger 1 (missed decision deadline) is explicitly
        // not enforceable in v1; §328 reaches only *revealed* votes.
        let c = case(
            Some(BUYER_WINS),
            vec![
                seat(1, Some(BUYER_WINS), 10_000),
                seat(2, None, 0),
                seat(3, None, 0),
            ],
        );
        let schedule = compute(&c).expect("decided");
        assert!(
            schedule.is_empty(),
            "silent seats must not be slashed while trigger 1 is unenforceable"
        );
    }

    #[test]
    fn an_undecided_case_condemns_nobody() {
        // The terminal even split: rounds exhausted, no verdict. There is
        // no consensus for a vote to have fallen outside of.
        let c = case(
            None,
            vec![
                seat(1, Some(BUYER_WINS), 10_000),
                seat(2, Some(MERCHANT_WINS), 10_000),
            ],
        );
        assert!(compute(&c).is_none());
    }

    #[test]
    fn a_unanimous_case_slashes_nobody() {
        let c = case(
            Some(MERCHANT_WINS),
            vec![
                seat(1, Some(MERCHANT_WINS), 10_000),
                seat(2, Some(MERCHANT_WINS), 15_000),
            ],
        );
        assert!(compute(&c).expect("decided").is_empty());
    }

    #[test]
    fn a_zero_weight_dissenter_is_still_recorded() {
        // The Sybil shape: a seat contributing nothing to the tally but
        // occupying a slot. Slashing zero stake costs it nothing; the
        // entry is what puts the attempt in the event log.
        let c = case(
            Some(BUYER_WINS),
            vec![
                seat(1, Some(BUYER_WINS), 10_000),
                seat(2, Some(MERCHANT_WINS), 0),
            ],
        );
        let schedule = compute(&c).expect("decided");
        assert_eq!(schedule.entries.len(), 1);
        assert_eq!(schedule.entries[0].weight, 0);
    }

    #[test]
    fn the_schedule_carries_the_outcome_it_dissented_from() {
        let c = case(Some(BUYER_WINS), vec![seat(1, Some(MERCHANT_WINS), 1_000)]);
        let schedule = compute(&c).expect("decided");
        assert_eq!(schedule.outcome, BUYER_WINS);
        assert_eq!(schedule.entries[0].revealed_outcome, MERCHANT_WINS);
        assert_eq!(schedule.reservation_id, 42);
        assert_eq!(schedule.round, 0);
    }

    #[test]
    fn a_round_is_slashed_at_most_once() {
        let mut applied = SlashedCases::new();
        assert!(!applied.is_applied(42, 0));
        assert!(applied.mark_applied(42, 0), "first mark is fresh");
        assert!(applied.is_applied(42, 0));
        assert!(
            !applied.mark_applied(42, 0),
            "a repeat must report false so the caller does not submit again"
        );
        assert_eq!(applied.len(), 1);
    }

    #[test]
    fn a_reopened_round_is_a_separate_verdict() {
        // Round 0 failed to decide and the case re-opened; round 1 decided.
        // Marking round 0 must not suppress round 1.
        let mut applied = SlashedCases::new();
        applied.mark_applied(42, 0);
        assert!(!applied.is_applied(42, 1));
        assert!(applied.mark_applied(42, 1));
        assert_eq!(applied.len(), 2);
    }

    #[test]
    fn distinct_cases_do_not_shadow_each_other() {
        let mut applied = SlashedCases::new();
        applied.mark_applied(42, 0);
        assert!(!applied.is_applied(43, 0));
    }

    #[test]
    fn the_gate_survives_a_round_trip() {
        // It has to be persisted to be worth anything — an in-memory-only
        // gate re-slashes everything after a restart.
        let mut applied = SlashedCases::new();
        applied.mark_applied(42, 0);
        applied.mark_applied(7, 2);
        let encoded = serde_json::to_string(&applied).expect("serialises");
        let restored: SlashedCases = serde_json::from_str(&encoded).expect("round-trips");
        assert_eq!(restored, applied);
        assert!(restored.is_applied(42, 0));
        assert!(restored.is_applied(7, 2));
    }
}
