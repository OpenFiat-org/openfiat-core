//! Turning an epoch's observations into what each node is owed.
//!
//! The computation is a pure function of `(epoch, observations,
//! eligibility, params)`. That is the point: any node holding the same
//! inputs derives the same schedule, so the authority that actually signs
//! `distribute_reward` can be checked rather than trusted. A schedule
//! that could only be produced by whoever gets to pay would be an
//! assertion, not a calculation.

use crate::liveness::LivenessLedger;
use crate::params::{InvalidParams, RewardParams};
use openfiat_types::PeerId;
use std::collections::HashMap;

/// What the paying node knows about a candidate beyond its own
/// observations: the two facts OFS-4100 §9.2 makes eligibility depend on.
///
/// `effective_stake` is the value decoded from the node's on-chain
/// `StakeAccount` — never a self-reported figure. `registered` is
/// presence in the OFS-1500 service registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Eligibility {
    pub effective_stake: u64,
    pub registered: bool,
}

/// One node's entitlement for one epoch.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RewardEntry {
    pub peer: PeerId,
    /// Base units of OPEN this node is owed for the epoch.
    pub amount: u64,
    /// The inputs that produced `amount`, carried so a reader can audit
    /// the result without re-deriving it.
    pub effective_stake: u64,
    pub connectivity_bps: u64,
    pub availability_bps: u64,
}

/// A complete, reproducible answer for one epoch.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RewardSchedule {
    pub epoch: u64,
    /// Emission actually apportioned, after the remaining-bucket cap.
    pub emission: u64,
    /// Entries with a non-zero amount, ordered by peer for determinism.
    pub entries: Vec<RewardEntry>,
    /// Emission left unassigned by integer truncation. Reported rather
    /// than quietly absorbed, so the sum of `entries` plus `dust` always
    /// equals `emission` and a reader can see nothing went missing.
    pub dust: u64,
}

impl RewardSchedule {
    /// Total across all entries.
    pub fn total(&self) -> u64 {
        self.entries.iter().map(|e| e.amount).sum()
    }
}

/// Computes the schedule for `epoch`.
///
/// `bootstrap_remaining` caps emission: once the Infrastructure bucket is
/// spent the pool is whatever the network earned, per OFS-4100 §9.1, and
/// this function will not emit past it.
pub fn compute(
    params: &RewardParams,
    ledger: &LivenessLedger,
    eligibility: &HashMap<PeerId, Eligibility>,
    epoch: u64,
    bootstrap_remaining: u64,
) -> Result<RewardSchedule, InvalidParams> {
    params.validate()?;

    let emission = params.per_epoch_emission.min(bootstrap_remaining);
    let observed = ledger.epoch(epoch);

    // Weight each eligible node. A node absent from `eligibility`, below
    // the stake floor, or unregistered contributes nothing and receives
    // nothing — the zero-weight participant is exactly the shape of the
    // Sybil problem this protocol has elsewhere, so it must never divide
    // into the pool.
    let mut weights: Vec<(PeerId, u128, u64, u64, u64)> = Vec::new();
    let mut total_weight: u128 = 0;

    for (peer, live) in &observed {
        let Some(el) = eligibility.get(peer) else {
            continue;
        };
        if !el.registered || el.effective_stake < params.min_stake {
            continue;
        }

        let connectivity_bps = live.connectivity_bps(params);
        let availability_bps = live.availability_bps(params);
        let weight = u128::from(el.effective_stake)
            * u128::from(connectivity_bps)
            * u128::from(availability_bps);
        if weight == 0 {
            continue;
        }

        total_weight += weight;
        weights.push((
            peer.clone(),
            weight,
            el.effective_stake,
            connectivity_bps,
            availability_bps,
        ));
    }

    if total_weight == 0 || emission == 0 {
        return Ok(RewardSchedule {
            epoch,
            emission,
            entries: Vec::new(),
            dust: emission,
        });
    }

    let mut entries = Vec::with_capacity(weights.len());
    let mut assigned: u64 = 0;
    for (peer, weight, effective_stake, connectivity_bps, availability_bps) in weights {
        // u128 throughout: emission is up to ~1e17 base units and weight
        // carries two bps factors, so the product overflows u64 easily.
        let amount = (u128::from(emission) * weight / total_weight) as u64;
        if amount == 0 {
            continue;
        }
        assigned = assigned.saturating_add(amount);
        entries.push(RewardEntry {
            peer,
            amount,
            effective_stake,
            connectivity_bps,
            availability_bps,
        });
    }

    entries.sort_by(|a, b| a.peer.as_bytes().cmp(b.peer.as_bytes()));

    Ok(RewardSchedule {
        epoch,
        emission,
        dust: emission.saturating_sub(assigned),
        entries,
    })
}

/// Which epochs have already been distributed.
///
/// Idempotence is enforced here rather than left to the caller because
/// the failure it prevents is asymmetric: skipping a payout is a delay,
/// paying twice is an unrecoverable transfer. `distribute_reward` credits
/// `pending_rewards` additively and has no notion of an epoch, so nothing
/// on-chain would reject a second run for the same epoch — the guard has
/// to live on this side.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PaidEpochs {
    paid: std::collections::BTreeSet<u64>,
}

impl PaidEpochs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_paid(&self, epoch: u64) -> bool {
        self.paid.contains(&epoch)
    }

    /// Marks `epoch` distributed. Returns `false` if it already was, so a
    /// caller can tell a fresh mark from a repeat without a prior read.
    pub fn mark_paid(&mut self, epoch: u64) -> bool {
        self.paid.insert(epoch)
    }

    pub fn highest_paid(&self) -> Option<u64> {
        self.paid.iter().next_back().copied()
    }
}

/// The epochs a node should pay out now: complete (strictly before the
/// current one) and not already paid.
///
/// Only settled epochs are returned. Paying the in-flight epoch would
/// distribute against a partial view of it, and since payment is
/// irreversible there is no correcting it once more observations arrive.
pub fn payable_epochs(
    params: &RewardParams,
    ledger: &LivenessLedger,
    paid: &PaidEpochs,
    now: openfiat_types::Timestamp,
) -> Vec<u64> {
    let current = params.epoch_index(now);
    ledger
        .epochs_held()
        .into_iter()
        .filter(|e| *e < current && !paid.is_paid(*e))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfiat_types::Timestamp;

    fn peer(tag: u8) -> PeerId {
        PeerId::from_bytes(vec![tag; 8])
    }

    const OPEN: u64 = 1_000_000_000;

    fn eligible(stake_open: u64) -> Eligibility {
        Eligibility {
            effective_stake: stake_open * OPEN,
            registered: true,
        }
    }

    /// Marks `p` live across the whole epoch, so availability is 1.0 and
    /// tests can isolate the factor they actually care about.
    fn fully_live(
        ledger: &mut LivenessLedger,
        params: &RewardParams,
        p: &PeerId,
        epoch: u64,
        rpc: bool,
    ) {
        let (start, _) = params.epoch_bounds(epoch);
        let slice = params.epoch_millis / u64::from(params.availability_buckets);
        for bucket in 0..params.availability_buckets {
            ledger.observe(
                params,
                p,
                Timestamp::from_millis(start + u64::from(bucket) * slice),
                rpc,
            );
        }
    }

    #[test]
    fn equal_nodes_split_the_emission_equally() {
        let params = RewardParams::default();
        let mut ledger = LivenessLedger::new();
        let (a, b) = (peer(1), peer(2));
        fully_live(&mut ledger, &params, &a, 5, true);
        fully_live(&mut ledger, &params, &b, 5, true);

        let el = HashMap::from([(a.clone(), eligible(5_000)), (b.clone(), eligible(5_000))]);
        let s = compute(&params, &ledger, &el, 5, u64::MAX).unwrap();

        assert_eq!(s.entries.len(), 2);
        assert_eq!(s.entries[0].amount, s.entries[1].amount);
        assert_eq!(s.total() + s.dust, s.emission, "nothing may vanish");
    }

    #[test]
    fn share_is_proportional_to_stake() {
        let params = RewardParams::default();
        let mut ledger = LivenessLedger::new();
        let (a, b) = (peer(1), peer(2));
        fully_live(&mut ledger, &params, &a, 5, true);
        fully_live(&mut ledger, &params, &b, 5, true);

        let el = HashMap::from([(a.clone(), eligible(3_000)), (b.clone(), eligible(1_000))]);
        let s = compute(&params, &ledger, &el, 5, u64::MAX).unwrap();

        let big = s.entries.iter().find(|e| e.peer == a).unwrap().amount;
        let small = s.entries.iter().find(|e| e.peer == b).unwrap().amount;
        assert_eq!(big / small, 3);
    }

    #[test]
    fn a_gossip_only_node_earns_four_tenths_of_an_otherwise_identical_rpc_node() {
        let params = RewardParams::default();
        let mut ledger = LivenessLedger::new();
        let (rpc, gossip) = (peer(1), peer(2));
        fully_live(&mut ledger, &params, &rpc, 9, true);
        fully_live(&mut ledger, &params, &gossip, 9, false);

        let el = HashMap::from([
            (rpc.clone(), eligible(5_000)),
            (gossip.clone(), eligible(5_000)),
        ]);
        let s = compute(&params, &ledger, &el, 9, u64::MAX).unwrap();

        let rpc_amount = s.entries.iter().find(|e| e.peer == rpc).unwrap().amount;
        let gossip_amount = s.entries.iter().find(|e| e.peer == gossip).unwrap().amount;
        // 0.4 / 1.0, within integer-division tolerance.
        let ratio_bps = (u128::from(gossip_amount) * 10_000 / u128::from(rpc_amount)) as u64;
        assert!(
            (3_999..=4_001).contains(&ratio_bps),
            "expected ~4000 bps, got {ratio_bps}"
        );
    }

    #[test]
    fn half_an_epoch_of_downtime_halves_the_share() {
        let params = RewardParams::default();
        let mut ledger = LivenessLedger::new();
        let (up, flaky) = (peer(1), peer(2));
        fully_live(&mut ledger, &params, &up, 4, true);

        let (start, _) = params.epoch_bounds(4);
        let slice = params.epoch_millis / u64::from(params.availability_buckets);
        for bucket in 0..params.availability_buckets / 2 {
            ledger.observe(
                &params,
                &flaky,
                Timestamp::from_millis(start + u64::from(bucket) * slice),
                true,
            );
        }

        let el = HashMap::from([
            (up.clone(), eligible(5_000)),
            (flaky.clone(), eligible(5_000)),
        ]);
        let s = compute(&params, &ledger, &el, 4, u64::MAX).unwrap();
        let up_amount = s.entries.iter().find(|e| e.peer == up).unwrap().amount;
        let flaky_amount = s.entries.iter().find(|e| e.peer == flaky).unwrap().amount;
        assert_eq!(up_amount / flaky_amount, 2);
    }

    #[test]
    fn an_unregistered_node_earns_nothing_however_live_or_staked() {
        let params = RewardParams::default();
        let mut ledger = LivenessLedger::new();
        let (good, unregistered) = (peer(1), peer(2));
        fully_live(&mut ledger, &params, &good, 5, true);
        fully_live(&mut ledger, &params, &unregistered, 5, true);

        let el = HashMap::from([
            (good.clone(), eligible(5_000)),
            (
                unregistered.clone(),
                Eligibility {
                    effective_stake: 500_000 * OPEN,
                    registered: false,
                },
            ),
        ]);
        let s = compute(&params, &ledger, &el, 5, u64::MAX).unwrap();
        assert_eq!(s.entries.len(), 1);
        assert_eq!(s.entries[0].peer, good);
    }

    #[test]
    fn a_zero_stake_node_earns_nothing_and_does_not_dilute_the_pool() {
        let params = RewardParams::default();
        let mut ledger = LivenessLedger::new();
        let (real, sybil) = (peer(1), peer(2));
        fully_live(&mut ledger, &params, &real, 5, true);
        fully_live(&mut ledger, &params, &sybil, 5, true);

        let solo = HashMap::from([(real.clone(), eligible(5_000))]);
        let alone = compute(&params, &ledger, &solo, 5, u64::MAX).unwrap();

        let with_sybil = HashMap::from([
            (real.clone(), eligible(5_000)),
            (
                sybil.clone(),
                Eligibility {
                    effective_stake: 0,
                    registered: true,
                },
            ),
        ]);
        let contested = compute(&params, &ledger, &with_sybil, 5, u64::MAX).unwrap();

        assert_eq!(contested.entries.len(), 1);
        assert_eq!(
            alone.entries[0].amount, contested.entries[0].amount,
            "a zero-stake peer must not reduce an honest node's share"
        );
    }

    #[test]
    fn a_node_below_the_stake_floor_earns_nothing() {
        let params = RewardParams::default();
        let mut ledger = LivenessLedger::new();
        let p = peer(1);
        fully_live(&mut ledger, &params, &p, 5, true);
        let el = HashMap::from([(
            p,
            Eligibility {
                effective_stake: params.min_stake - 1,
                registered: true,
            },
        )]);
        assert!(
            compute(&params, &ledger, &el, 5, u64::MAX)
                .unwrap()
                .entries
                .is_empty()
        );
    }

    #[test]
    fn emission_is_capped_by_what_remains_in_the_bootstrap_bucket() {
        let params = RewardParams::default();
        let mut ledger = LivenessLedger::new();
        let p = peer(1);
        fully_live(&mut ledger, &params, &p, 5, true);
        let el = HashMap::from([(p, eligible(5_000))]);

        let remaining = 100 * OPEN;
        let s = compute(&params, &ledger, &el, 5, remaining).unwrap();
        assert_eq!(s.emission, remaining);
        assert!(s.total() <= remaining);
    }

    #[test]
    fn an_exhausted_bucket_pays_nothing_rather_than_erroring() {
        let params = RewardParams::default();
        let mut ledger = LivenessLedger::new();
        let p = peer(1);
        fully_live(&mut ledger, &params, &p, 5, true);
        let el = HashMap::from([(p, eligible(5_000))]);
        let s = compute(&params, &ledger, &el, 5, 0).unwrap();
        assert_eq!(s.emission, 0);
        assert!(s.entries.is_empty());
    }

    #[test]
    fn the_same_inputs_always_produce_the_same_schedule() {
        let params = RewardParams::default();
        let mut ledger = LivenessLedger::new();
        for tag in 1..=5u8 {
            fully_live(&mut ledger, &params, &peer(tag), 5, tag % 2 == 0);
        }
        let el: HashMap<_, _> = (1..=5u8)
            .map(|t| (peer(t), eligible(1_000 * u64::from(t))))
            .collect();

        let first = compute(&params, &ledger, &el, 5, u64::MAX).unwrap();
        let second = compute(&params, &ledger, &el, 5, u64::MAX).unwrap();
        assert_eq!(
            first, second,
            "a schedule anyone can reproduce is what makes the authority checkable"
        );
    }

    #[test]
    fn an_epoch_is_payable_once_and_then_never_again() {
        let params = RewardParams::default();
        let mut ledger = LivenessLedger::new();
        fully_live(&mut ledger, &params, &peer(1), 5, true);

        let mut paid = PaidEpochs::new();
        let now = Timestamp::from_millis(params.epoch_bounds(9).0);

        assert_eq!(payable_epochs(&params, &ledger, &paid, now), vec![5]);
        assert!(paid.mark_paid(5), "first mark is fresh");
        assert!(!paid.mark_paid(5), "second mark reports a repeat");
        assert!(
            payable_epochs(&params, &ledger, &paid, now).is_empty(),
            "double-paying is unrecoverable, so a paid epoch must never resurface"
        );
    }

    #[test]
    fn the_in_flight_epoch_is_never_payable() {
        let params = RewardParams::default();
        let mut ledger = LivenessLedger::new();
        fully_live(&mut ledger, &params, &peer(1), 12, true);

        let mid_epoch = Timestamp::from_millis(params.epoch_bounds(12).0 + 1_000);
        assert!(
            payable_epochs(&params, &ledger, &PaidEpochs::new(), mid_epoch).is_empty(),
            "paying a partial view of an epoch cannot be corrected afterwards"
        );
    }
}
