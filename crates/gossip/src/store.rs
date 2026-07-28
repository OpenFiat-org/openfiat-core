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
        self.store.get(COLUMN_FAMILY, id.as_bytes()).ok().flatten().is_some()
    }

    pub fn get(&self, id: &EventId) -> Option<EventEnvelope> {
        let bytes = self.store.get(COLUMN_FAMILY, id.as_bytes()).ok().flatten()?;
        wire::from_bytes(&bytes).ok()
    }

    /// Store an event. Idempotent: storing the same ID twice just
    /// overwrites with (necessarily identical, per §5) content.
    pub fn put(&self, event: &EventEnvelope) {
        if let Ok(bytes) = wire::to_bytes(event) {
            let _ = self.store.put(COLUMN_FAMILY, event.id.as_bytes(), &bytes);
        }
    }

    pub fn all(&self) -> Vec<EventEnvelope> {
        self.store.iter_prefix(COLUMN_FAMILY, &[]).unwrap_or_default().into_iter().filter_map(|(_, value)| wire::from_bytes(&value).ok()).collect()
    }

    /// Every stored event on a channel `subscription` accepts — the set a
    /// newly (re)connected peer needs to catch up on (§17, §22).
    pub fn all_for_subscription(&self, subscription: &Subscription) -> Vec<EventEnvelope> {
        self.all().into_iter().filter(|event| subscription.accepts(Channel::for_ofs_spec(event.ofs_spec))).collect()
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
}
