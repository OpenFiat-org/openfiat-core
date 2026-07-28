//! The persistent local peer cache (OFS-1100 §7): "Every node maintains a
//! persistent peer database. The peer database SHOULD survive restarts."
//!
//! Generic over `openfiat_storage::KvStore` so tests run against
//! `openfiat_storage::mem::MemoryStore` and real nodes run against
//! `openfiat_database::Database` (RocksDB) — "the reference implementation
//! stores this information inside RocksDB", per §7, without this crate
//! depending on RocksDB directly.

use crate::record::PeerRecord;
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{PeerId, Timestamp};
use std::time::Duration;

const COLUMN_FAMILY: &str = "peers";

/// A persistent cache of everything known about discovered peers.
pub struct PeerCache<S> {
    store: S,
}

impl<S: KvStore> PeerCache<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }

    pub fn upsert(&self, record: &PeerRecord) -> Result<(), S::Error> {
        let bytes = wire::to_bytes(record).expect("PeerRecord always serializes");
        self.store
            .put(COLUMN_FAMILY, record.peer_id.as_bytes(), &bytes)
    }

    pub fn get(&self, peer_id: &PeerId) -> Result<Option<PeerRecord>, S::Error> {
        let bytes = self.store.get(COLUMN_FAMILY, peer_id.as_bytes())?;
        Ok(bytes.and_then(|bytes| wire::from_bytes(&bytes).ok()))
    }

    pub fn remove(&self, peer_id: &PeerId) -> Result<(), S::Error> {
        self.store.delete(COLUMN_FAMILY, peer_id.as_bytes())
    }

    /// Every cached peer record.
    pub fn all(&self) -> Result<Vec<PeerRecord>, S::Error> {
        let entries = self.store.iter_prefix(COLUMN_FAMILY, &[])?;
        Ok(entries
            .into_iter()
            .filter_map(|(_, value)| wire::from_bytes(&value).ok())
            .collect())
    }

    /// Remove peers not seen within `max_age` (§23 peer expiration).
    pub fn expire_stale(&self, max_age: Duration) -> Result<usize, S::Error> {
        let cutoff = Timestamp::now()
            .as_millis()
            .saturating_sub(max_age.as_millis() as u64);
        let stale: Vec<PeerId> = self
            .all()?
            .into_iter()
            .filter(|record| record.last_seen.as_millis() < cutoff)
            .map(|record| record.peer_id)
            .collect();
        let count = stale.len();
        for peer_id in &stale {
            self.remove(peer_id)?;
        }
        Ok(count)
    }

    /// Up to `limit` peers, healthiest first: fewest failures, then most
    /// successes, then lowest latency (§11 peer selection, §13 connection
    /// replacement).
    pub fn healthiest(&self, limit: usize) -> Result<Vec<PeerRecord>, S::Error> {
        let mut all = self.all()?;
        all.sort_by(|a, b| {
            a.failures
                .cmp(&b.failures)
                .then_with(|| b.successes.cmp(&a.successes))
                .then_with(|| {
                    a.latency_ms
                        .unwrap_or(u32::MAX)
                        .cmp(&b.latency_ms.unwrap_or(u32::MAX))
                })
        });
        all.truncate(limit);
        Ok(all)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfiat_storage::mem::MemoryStore;
    use openfiat_types::PublicKey;

    fn record(seed: u8) -> PeerRecord {
        PeerRecord::new(
            PeerId::from_bytes(vec![seed]),
            PublicKey::from_bytes([seed; 32]),
            vec![format!("/ip4/127.0.0.1/udp/{}/quic-v1", 4000 + seed as u16)],
            "1.0.0".to_string(),
            vec![1000],
            vec![],
        )
    }

    #[test]
    fn upsert_then_get_round_trips() {
        let cache = PeerCache::new(MemoryStore::new());
        let record = record(1);
        cache.upsert(&record).unwrap();
        assert_eq!(cache.get(&record.peer_id).unwrap(), Some(record));
    }

    #[test]
    fn remove_deletes_the_record() {
        let cache = PeerCache::new(MemoryStore::new());
        let record = record(1);
        cache.upsert(&record).unwrap();
        cache.remove(&record.peer_id).unwrap();
        assert_eq!(cache.get(&record.peer_id).unwrap(), None);
    }

    #[test]
    fn expire_stale_removes_only_old_records() {
        let cache = PeerCache::new(MemoryStore::new());
        let mut old = record(1);
        old.last_seen = Timestamp::from_millis(0);
        let fresh = record(2);
        cache.upsert(&old).unwrap();
        cache.upsert(&fresh).unwrap();

        let removed = cache.expire_stale(Duration::from_secs(60)).unwrap();
        assert_eq!(removed, 1);
        assert_eq!(cache.get(&old.peer_id).unwrap(), None);
        assert_eq!(cache.get(&fresh.peer_id).unwrap(), Some(fresh));
    }

    #[test]
    fn healthiest_orders_by_failures_then_successes_then_latency() {
        let cache = PeerCache::new(MemoryStore::new());

        let mut bad = record(1);
        bad.failures = 5;
        let mut good = record(2);
        good.successes = 10;
        good.latency_ms = Some(20);
        let mut best = record(3);
        best.successes = 10;
        best.latency_ms = Some(5);

        cache.upsert(&bad).unwrap();
        cache.upsert(&good).unwrap();
        cache.upsert(&best).unwrap();

        let ranked = cache.healthiest(2).unwrap();
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].peer_id, best.peer_id);
        assert_eq!(ranked[1].peer_id, good.peer_id);
    }
}
