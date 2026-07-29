//! What one node can honestly say about whether another was up.
//!
//! # Why presence-per-slice, and not an event count
//!
//! The obvious metric — count each peer's `BlockhashAnnounced` events —
//! is wrong here, and the reason is worth stating because it is not
//! obvious from the announcement code alone.
//!
//! OFS-4300 §6's amplification control (`openfiat-chain`'s forward
//! filter) suppresses *re-forwarding* an announcement whose
//! `(blockhash, slot)` this node has already seen. Received events are
//! still handled — `notify()` runs before `should_forward()` — so the
//! filter costs nothing locally. What it costs is **reach**: an
//! announcement from a peer that is consistently a little slower, or a
//! little further away in the topology, is dropped by the first
//! intermediate node that already relayed the same content, and never
//! arrives here at all.
//!
//! Counting announcements would therefore measure *who announced first*,
//! and would pay a well-connected node more than an equally available
//! one behind a slower link. That is a topology tax, not a service
//! measurement.
//!
//! So this ledger asks a weaker question that the available data can
//! actually answer: **in each slice of the epoch, did we hear anything at
//! all, signed by this peer?** Losing a race costs a peer nothing as long
//! as it is heard from at some point in the slice, and — because a slice
//! saturates at one — flooding the network earns nothing extra either.
//!
//! Any signed event counts toward presence, not just chain-bridge
//! announcements, which is what OFS-4100 §9.2 means by "and gossip
//! participation": the dedup filter is specific to
//! `BlockhashAnnounced`, so a peer's other traffic is an undistorted
//! liveness signal.
//!
//! # What this deliberately does not prove
//!
//! Connectivity here means "we saw this peer originate a chain-bridge
//! announcement". That is a *lower bound*, and it is spoofable: a
//! `GossipOnly` node can take a `(blockhash, slot)` it heard from
//! someone else and re-announce it under its own signature. Nothing in
//! the envelope distinguishes that from a genuine observation.
//!
//! Closing it needs something this protocol does not have — a
//! challenge-response, or an on-chain attestation tying the announcement
//! to the announcer. OFS-4100 §9.2 says as much ("not solved by this
//! specification"), and this module does not pretend otherwise. The
//! honest framing is that connectivity is claimed-and-plausible rather
//! than proven, and the 0.4 floor for gossip-only nodes limits what the
//! lie is worth rather than preventing it.

use crate::params::RewardParams;
use openfiat_types::{PeerId, Timestamp};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// What was observed of one peer during one epoch.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PeerLiveness {
    /// Indices of the epoch slices this peer was heard in at all.
    /// A set, not a counter: presence saturates per slice.
    pub buckets_seen: BTreeSet<u32>,
    /// Whether a chain-bridge announcement from this peer was observed.
    /// See the module doc on what this does and does not establish.
    pub announced_blockhash: bool,
}

impl PeerLiveness {
    /// Fraction of the epoch this peer was observed live, in basis
    /// points, saturating at 1.0.
    pub fn availability_bps(&self, params: &RewardParams) -> u64 {
        let buckets = params.availability_buckets.max(1) as u64;
        let seen = self.buckets_seen.len() as u64;
        (seen.min(buckets) * crate::params::BPS_DENOMINATOR) / buckets
    }

    /// The §9.2 connectivity multiplier for this peer, in basis points.
    pub fn connectivity_bps(&self, params: &RewardParams) -> u64 {
        if self.announced_blockhash {
            params.connectivity_rpc_bps
        } else {
            params.connectivity_gossip_bps
        }
    }
}

/// One epoch's observations, as seen by this node and no other.
///
/// Deliberately per-epoch and append-only: an observation cannot be
/// retracted, and an epoch's ledger stops changing once the epoch ends,
/// which is what makes a schedule computed from it reproducible.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LivenessLedger {
    epochs: BTreeMap<u64, HashMap<PeerId, PeerLiveness>>,
}

impl LivenessLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one observed event.
    ///
    /// `origin` is the envelope's own `origin` field, which the gossip
    /// layer has already verified against the envelope signature — this
    /// module never takes a peer's word for who it is.
    pub fn observe(
        &mut self,
        params: &RewardParams,
        origin: &PeerId,
        observed_at: Timestamp,
        is_blockhash_announcement: bool,
    ) {
        let epoch = params.epoch_index(observed_at);
        let (start, _) = params.epoch_bounds(epoch);
        let elapsed = observed_at.as_millis().saturating_sub(start);
        let slice = params.epoch_millis / u64::from(params.availability_buckets.max(1));
        let bucket = (elapsed / slice.max(1)) as u32;

        let entry = self
            .epochs
            .entry(epoch)
            .or_default()
            .entry(origin.clone())
            .or_default();
        entry
            .buckets_seen
            .insert(bucket.min(params.availability_buckets.saturating_sub(1)));
        entry.announced_blockhash |= is_blockhash_announcement;
    }

    /// Observations for `epoch`, empty if nothing was heard.
    pub fn epoch(&self, epoch: u64) -> HashMap<PeerId, PeerLiveness> {
        self.epochs.get(&epoch).cloned().unwrap_or_default()
    }

    /// Drops observations for epochs at or before `epoch`, so a
    /// long-running node's ledger does not grow without bound once an
    /// epoch has been paid and can no longer be recomputed.
    pub fn prune_through(&mut self, epoch: u64) {
        self.epochs.retain(|e, _| *e > epoch);
    }

    /// Epochs currently held, oldest first.
    pub fn epochs_held(&self) -> Vec<u64> {
        self.epochs.keys().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(tag: u8) -> PeerId {
        PeerId::from_bytes(vec![tag; 8])
    }

    fn at(params: &RewardParams, epoch: u64, bucket: u32) -> Timestamp {
        let (start, _) = params.epoch_bounds(epoch);
        let slice = params.epoch_millis / u64::from(params.availability_buckets);
        Timestamp::from_millis(start + u64::from(bucket) * slice)
    }

    #[test]
    fn presence_saturates_per_slice_so_flooding_earns_nothing_extra() {
        let params = RewardParams::default();
        let mut ledger = LivenessLedger::new();
        let chatty = peer(1);
        let quiet = peer(2);

        for _ in 0..500 {
            ledger.observe(&params, &chatty, at(&params, 7, 0), false);
        }
        ledger.observe(&params, &quiet, at(&params, 7, 0), false);

        let epoch = ledger.epoch(7);
        assert_eq!(
            epoch[&chatty].availability_bps(&params),
            epoch[&quiet].availability_bps(&params),
            "500 events in one slice must score the same as one"
        );
    }

    #[test]
    fn availability_tracks_the_share_of_slices_a_peer_was_heard_in() {
        let params = RewardParams::default();
        let mut ledger = LivenessLedger::new();
        let p = peer(1);
        for bucket in 0..12 {
            ledger.observe(&params, &p, at(&params, 3, bucket), false);
        }
        // 12 of 24 slices.
        assert_eq!(ledger.epoch(3)[&p].availability_bps(&params), 5_000);
    }

    #[test]
    fn a_peer_heard_in_every_slice_reaches_exactly_one() {
        let params = RewardParams::default();
        let mut ledger = LivenessLedger::new();
        let p = peer(1);
        for bucket in 0..params.availability_buckets {
            ledger.observe(&params, &p, at(&params, 1, bucket), false);
        }
        assert_eq!(
            ledger.epoch(1)[&p].availability_bps(&params),
            crate::params::BPS_DENOMINATOR
        );
    }

    #[test]
    fn any_gossip_counts_toward_presence_not_just_chain_announcements() {
        let params = RewardParams::default();
        let mut ledger = LivenessLedger::new();
        let p = peer(4);
        ledger.observe(&params, &p, at(&params, 2, 0), false);
        let live = &ledger.epoch(2)[&p];
        assert!(live.availability_bps(&params) > 0);
        assert!(
            !live.announced_blockhash,
            "presence must not imply chain connectivity"
        );
        assert_eq!(
            live.connectivity_bps(&params),
            params.connectivity_gossip_bps
        );
    }

    #[test]
    fn observing_an_announcement_raises_connectivity_to_the_rpc_multiplier() {
        let params = RewardParams::default();
        let mut ledger = LivenessLedger::new();
        let p = peer(5);
        ledger.observe(&params, &p, at(&params, 2, 0), true);
        assert_eq!(
            ledger.epoch(2)[&p].connectivity_bps(&params),
            params.connectivity_rpc_bps
        );
    }

    #[test]
    fn observations_are_filed_under_the_epoch_they_happened_in() {
        let params = RewardParams::default();
        let mut ledger = LivenessLedger::new();
        let p = peer(6);
        ledger.observe(&params, &p, at(&params, 10, 0), false);
        ledger.observe(&params, &p, at(&params, 11, 0), false);
        assert_eq!(ledger.epochs_held(), vec![10, 11]);
        assert!(ledger.epoch(12).is_empty());
    }

    #[test]
    fn pruning_drops_paid_epochs_and_keeps_later_ones() {
        let params = RewardParams::default();
        let mut ledger = LivenessLedger::new();
        let p = peer(7);
        for epoch in 1..=4 {
            ledger.observe(&params, &p, at(&params, epoch, 0), false);
        }
        ledger.prune_through(2);
        assert_eq!(ledger.epochs_held(), vec![3, 4]);
    }
}
