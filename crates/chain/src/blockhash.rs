//! Blockhash announcement dedup and recency selection (OFS-4300 §6).
//!
//! Two independent mechanisms, both required:
//!
//! - **Amplification control**: [`BlockhashCache::observe`] tells the
//!   caller whether this `(blockhash, slot)` pair has been seen before,
//!   so a gossip layer only rebroadcasts the first announcement of a
//!   given pair — bounding fan-out regardless of how many independent
//!   RPC-connected nodes announce the same real-world blockhash.
//! - **Recency selection**: [`BlockhashCache::current`] always returns
//!   the highest-slot, not-yet-expired blockhash seen so far — a node's
//!   own view for constructing or forwarding transactions. The first
//!   pair a node ever saw is not necessarily the one it should still be
//!   using; both mechanisms are needed together (§6).

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Solana's own blockhash validity window is ~150 slots (~60-90s at
/// ~400ms/slot). `[PROPOSED — NEEDS SIGN-OFF]` a fixed wall-clock
/// duration is used here rather than tracking the cluster's actual slot
/// cadence, which this crate has no independent way to observe without
/// itself being RPC-connected.
pub const BLOCKHASH_VALIDITY: Duration = Duration::from_secs(75);

#[derive(Debug, Clone)]
struct Observed {
    slot: u64,
    observed_at: Instant,
}

/// Tracks blockhash announcements observed from gossip (or, on an
/// RPC-connected node, from its own polling loop before it announces).
#[derive(Debug)]
pub struct BlockhashCache {
    /// First-seen tracking, keyed by content — not event id — per §6's
    /// amplification-control mechanism.
    seen: HashMap<(String, u64), Instant>,
    /// The highest-slot, not-yet-expired blockhash observed so far.
    current: Option<(String, Observed)>,
    validity: Duration,
}

impl Default for BlockhashCache {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockhashCache {
    pub fn new() -> Self {
        Self {
            seen: HashMap::new(),
            current: None,
            validity: BLOCKHASH_VALIDITY,
        }
    }

    /// A cache with a custom validity window, for tests that need to
    /// observe expiry without waiting out the real ~75s default.
    #[cfg(test)]
    fn with_validity(validity: Duration) -> Self {
        Self {
            seen: HashMap::new(),
            current: None,
            validity,
        }
    }

    /// Records an observed `(blockhash, slot)` pair. Returns `true` the
    /// first time this exact pair is observed (the caller should
    /// rebroadcast it), `false` on every subsequent observation of the
    /// same pair (the caller must not rebroadcast it).
    pub fn observe(&mut self, blockhash: &str, slot: u64) -> bool {
        self.prune_expired();

        let now = Instant::now();
        let first_seen = !self.seen.contains_key(&(blockhash.to_string(), slot));
        self.seen.insert((blockhash.to_string(), slot), now);

        let is_newer = match &self.current {
            Some((_, observed)) => slot > observed.slot,
            None => true,
        };
        if is_newer {
            self.current = Some((
                blockhash.to_string(),
                Observed {
                    slot,
                    observed_at: now,
                },
            ));
        }

        first_seen
    }

    /// The current blockhash to build or forward a transaction against,
    /// if one within the validity window has been observed.
    pub fn current(&self) -> Option<(&str, u64)> {
        self.current.as_ref().and_then(|(hash, observed)| {
            (observed.observed_at.elapsed() < self.validity)
                .then_some((hash.as_str(), observed.slot))
        })
    }

    fn prune_expired(&mut self) {
        let validity = self.validity;
        self.seen
            .retain(|_, observed_at| observed_at.elapsed() < validity);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_observation_of_a_pair_reports_itself_as_new() {
        let mut cache = BlockhashCache::new();
        assert!(cache.observe("hash-a", 100));
    }

    #[test]
    fn repeated_observation_of_the_same_pair_is_not_new() {
        let mut cache = BlockhashCache::new();
        assert!(cache.observe("hash-a", 100));
        assert!(!cache.observe("hash-a", 100));
        assert!(!cache.observe("hash-a", 100));
    }

    #[test]
    fn a_different_slot_for_the_same_blockhash_string_is_a_distinct_pair() {
        let mut cache = BlockhashCache::new();
        assert!(cache.observe("hash-a", 100));
        assert!(cache.observe("hash-a", 101));
    }

    #[test]
    fn current_tracks_the_highest_slot_seen_even_if_seen_second() {
        let mut cache = BlockhashCache::new();
        cache.observe("hash-old", 100);
        cache.observe("hash-new", 200);
        assert_eq!(cache.current(), Some(("hash-new", 200)));
    }

    #[test]
    fn an_out_of_order_older_slot_does_not_override_the_current_choice() {
        let mut cache = BlockhashCache::new();
        cache.observe("hash-new", 200);
        cache.observe("hash-old", 100);
        assert_eq!(cache.current(), Some(("hash-new", 200)));
    }

    #[test]
    fn no_current_blockhash_before_anything_is_observed() {
        let cache = BlockhashCache::new();
        assert_eq!(cache.current(), None);
    }

    #[test]
    fn an_expired_blockhash_is_no_longer_returned_as_current() {
        let mut cache = BlockhashCache::with_validity(Duration::from_millis(20));
        cache.observe("hash-a", 100);
        assert!(cache.current().is_some());
        std::thread::sleep(Duration::from_millis(40));
        assert_eq!(cache.current(), None);
    }

    #[test]
    fn an_expired_pair_is_treated_as_new_again_if_re_observed() {
        let mut cache = BlockhashCache::with_validity(Duration::from_millis(20));
        assert!(cache.observe("hash-a", 100));
        std::thread::sleep(Duration::from_millis(40));
        assert!(
            cache.observe("hash-a", 100),
            "expired entries are pruned, so re-observing looks new"
        );
    }
}
