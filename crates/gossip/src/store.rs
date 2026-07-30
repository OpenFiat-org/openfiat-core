//! The event store (OGP §10): "Every received event SHALL be temporarily
//! stored... duplicate detection, recovery, replay prevention, late peer
//! synchronization." Generic over `KvStore`, matching the pattern
//! established by `openfiat-discovery`'s peer cache — tests run against
//! `MemoryStore`, real nodes against RocksDB.

use crate::channel::{Channel, Subscription};
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{EventEnvelope, EventId};

const COLUMN_FAMILY: &str = "gossip_events";

pub struct EventStore<S> {
    store: S,
}

impl<S: KvStore> EventStore<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }

    pub fn contains(&self, id: &EventId) -> bool {
        self.store
            .get(COLUMN_FAMILY, id.as_bytes())
            .ok()
            .flatten()
            .is_some()
    }

    pub fn get(&self, id: &EventId) -> Option<EventEnvelope> {
        let bytes = self
            .store
            .get(COLUMN_FAMILY, id.as_bytes())
            .ok()
            .flatten()?;
        wire::from_bytes(&bytes).ok()
    }

    /// Store an event. Idempotent: storing the same ID twice just
    /// overwrites with (necessarily identical, per §5) content.
    pub fn put(&self, event: &EventEnvelope) {
        if let Ok(bytes) = wire::to_bytes(event) {
            let _ = self.store.put(COLUMN_FAMILY, event.id.as_bytes(), &bytes);
        }
    }

    /// Drops events older than `cutoff`, returning how many went.
    ///
    /// # The log is a buffer, not the state
    ///
    /// It exists for three things (OGP §10): duplicate detection, replay
    /// prevention, and catching up a peer that was away. None of them is
    /// "remember everything that ever happened" — the domain registries
    /// hold the state, and a snapshot is how a node far behind gets it.
    /// Left unpruned this column family grows without bound and is, on a
    /// busy node, the largest thing on disk, because it holds every
    /// event's full payload alongside the record that event produced.
    ///
    /// # What pruning costs, stated rather than assumed
    ///
    /// A peer that was offline longer than the window can no longer be
    /// caught up from this log and must bootstrap from a snapshot. That is
    /// what snapshots are for, and why the window is set well above any
    /// ordinary restart.
    ///
    /// Duplicate detection also stops covering pruned events, so an event
    /// older than the window that is re-gossiped will be applied again
    /// rather than recognised. That is survivable because every registry
    /// enforces its own idempotence — `apply_publish` rejects a duplicate
    /// id, the settlement and reservation state machines reject invalid
    /// transitions — so re-application is a no-op rather than a
    /// double-count. It is survivable, not free: the window must stay far
    /// enough above the replay-protection requirement in
    /// `docs/architecture.md` (24h) that this is a theoretical path and
    /// not a routine one.
    pub fn prune_before(&self, cutoff: openfiat_types::Timestamp) -> usize {
        let mut dropped = 0;
        for (key, value) in self
            .store
            .iter_prefix(COLUMN_FAMILY, &[])
            .unwrap_or_default()
        {
            let Ok(event) = wire::from_bytes::<EventEnvelope>(&value) else {
                // Undecodable is unusable: it can neither be served for
                // recovery nor answer a dedup question, so it is only
                // taking up space.
                let _ = self.store.delete(COLUMN_FAMILY, &key);
                dropped += 1;
                continue;
            };
            if event.timestamp < cutoff && self.store.delete(COLUMN_FAMILY, &key).is_ok() {
                dropped += 1;
            }
        }
        dropped
    }

    pub fn all(&self) -> Vec<EventEnvelope> {
        self.store
            .iter_prefix(COLUMN_FAMILY, &[])
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(_, value)| wire::from_bytes(&value).ok())
            .collect()
    }

    /// Every stored event on a channel `subscription` accepts — the set a
    /// newly (re)connected peer needs to catch up on (§17, §22).
    pub fn all_for_subscription(&self, subscription: &Subscription) -> Vec<EventEnvelope> {
        self.all()
            .into_iter()
            .filter(|event| subscription.accepts(Channel::for_ofs_spec(event.ofs_spec)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfiat_storage::mem::MemoryStore;
    use openfiat_types::{Priority, Signature, Timestamp};

    fn event(id_byte: u8, ofs_spec: u16) -> EventEnvelope {
        EventEnvelope {
            id: EventId::from_bytes([id_byte; 32]),
            event_type: openfiat_types::EventType::new("AdvertisementCreated").unwrap(),
            ofs_spec,
            version: 1,
            origin: openfiat_types::PeerId::from_bytes(vec![1]),
            timestamp: Timestamp::now(),
            ttl: 8,
            priority: Priority::Advertisement,
            signature: Signature::from_bytes([0u8; 64]),
            payload: vec![],
        }
    }

    #[test]
    fn put_then_get_round_trips() {
        let store = EventStore::new(MemoryStore::new());
        let event = event(1, 2100);
        store.put(&event);
        assert_eq!(store.get(&event.id), Some(event));
    }

    #[test]
    fn contains_reflects_what_has_been_stored() {
        let store = EventStore::new(MemoryStore::new());
        let event = event(1, 2100);
        assert!(!store.contains(&event.id));
        store.put(&event);
        assert!(store.contains(&event.id));
    }

    #[test]
    fn all_for_subscription_filters_by_channel() {
        let store = EventStore::new(MemoryStore::new());
        let marketplace_event = event(1, 2100);
        let oracle_event = event(2, 7000);
        store.put(&marketplace_event);
        store.put(&oracle_event);

        let subscription = Subscription::Only(vec![Channel::Oracle]);
        let visible = store.all_for_subscription(&subscription);
        assert_eq!(visible, vec![oracle_event]);
    }

    /// A recovery buffer that is never pruned is, on a busy node, the
    /// largest thing on disk — it holds every event's full payload
    /// alongside the record that event already produced.
    mod pruning {
        use super::*;
        use openfiat_types::Timestamp;

        fn at(millis: u64) -> EventEnvelope {
            let mut event = event(1, 2100);
            event.id = EventId::from_bytes([(millis % 251) as u8; 32]);
            event.timestamp = Timestamp::from_millis(millis);
            event
        }

        #[test]
        fn drops_what_is_older_than_the_cutoff_and_keeps_the_rest() {
            let store = EventStore::new(MemoryStore::new());
            let old = at(1_000);
            let recent = at(9_000);
            store.put(&old);
            store.put(&recent);

            assert_eq!(store.prune_before(Timestamp::from_millis(5_000)), 1);
            assert!(!store.contains(&old.id));
            assert!(store.contains(&recent.id), "the window must survive");
        }

        #[test]
        fn an_event_exactly_at_the_cutoff_is_kept() {
            // The window is inclusive of its own edge, so a cutoff
            // computed from "now minus the window" never drops the event
            // that is exactly that old.
            let store = EventStore::new(MemoryStore::new());
            let edge = at(5_000);
            store.put(&edge);
            assert_eq!(store.prune_before(Timestamp::from_millis(5_000)), 0);
            assert!(store.contains(&edge.id));
        }

        #[test]
        fn pruning_is_idempotent() {
            let store = EventStore::new(MemoryStore::new());
            store.put(&at(1_000));
            assert_eq!(store.prune_before(Timestamp::from_millis(5_000)), 1);
            assert_eq!(store.prune_before(Timestamp::from_millis(5_000)), 0);
        }

        #[test]
        fn a_pruned_event_no_longer_answers_a_duplicate_check() {
            // Stated as a test because it is the real cost of pruning: an
            // old event re-gossiped after its window is applied again
            // rather than recognised. Every registry enforces its own
            // idempotence, which is what makes that survivable.
            let store = EventStore::new(MemoryStore::new());
            let old = at(1_000);
            store.put(&old);
            assert!(store.contains(&old.id));

            store.prune_before(Timestamp::from_millis(5_000));
            assert!(!store.contains(&old.id));
        }

        #[test]
        fn recovery_after_pruning_serves_only_the_window() {
            // A peer away longer than the window cannot be caught up from
            // this log and must bootstrap from a snapshot.
            let store = EventStore::new(MemoryStore::new());
            store.put(&at(1_000));
            store.put(&at(9_000));
            store.prune_before(Timestamp::from_millis(5_000));

            let recoverable = store.all_for_subscription(&Subscription::All);
            assert_eq!(recoverable.len(), 1);
            assert_eq!(recoverable[0].timestamp, Timestamp::from_millis(9_000));
        }

        #[test]
        fn an_empty_log_prunes_to_nothing_without_erroring() {
            let store = EventStore::new(MemoryStore::new());
            assert_eq!(store.prune_before(Timestamp::from_millis(5_000)), 0);
        }
    }
}
