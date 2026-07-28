//! An in-memory [`KvStore`], for tests that need real read/write/delete/
//! iteration semantics without a RocksDB instance.

use crate::{Entry, KvStore};
use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::Mutex;

/// One column family's contents, keyed and ordered by key.
type ColumnFamilyContents = BTreeMap<Vec<u8>, Vec<u8>>;

/// An in-memory, thread-safe [`KvStore`]. Not persisted; contents are lost
/// when dropped.
#[derive(Default)]
pub struct MemoryStore {
    column_families: Mutex<BTreeMap<String, ColumnFamilyContents>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl KvStore for MemoryStore {
    type Error = Infallible;

    fn get(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, Infallible> {
        let column_families = self
            .column_families
            .lock()
            .expect("MemoryStore mutex poisoned");
        Ok(column_families
            .get(cf)
            .and_then(|entries| entries.get(key))
            .cloned())
    }

    fn put(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), Infallible> {
        let mut column_families = self
            .column_families
            .lock()
            .expect("MemoryStore mutex poisoned");
        column_families
            .entry(cf.to_string())
            .or_default()
            .insert(key.to_vec(), value.to_vec());
        Ok(())
    }

    fn delete(&self, cf: &str, key: &[u8]) -> Result<(), Infallible> {
        let mut column_families = self
            .column_families
            .lock()
            .expect("MemoryStore mutex poisoned");
        if let Some(entries) = column_families.get_mut(cf) {
            entries.remove(key);
        }
        Ok(())
    }

    fn iter_prefix(&self, cf: &str, prefix: &[u8]) -> Result<Vec<Entry>, Infallible> {
        let column_families = self
            .column_families
            .lock()
            .expect("MemoryStore mutex poisoned");
        let Some(entries) = column_families.get(cf) else {
            return Ok(Vec::new());
        };
        Ok(entries
            .range(prefix.to_vec()..)
            .take_while(|(key, _)| key.starts_with(prefix))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_value() {
        let store = MemoryStore::new();
        store.put("events", b"key", b"value").unwrap();
        assert_eq!(
            store.get("events", b"key").unwrap(),
            Some(b"value".to_vec())
        );
    }

    #[test]
    fn delete_removes_the_key() {
        let store = MemoryStore::new();
        store.put("events", b"key", b"value").unwrap();
        store.delete("events", b"key").unwrap();
        assert_eq!(store.get("events", b"key").unwrap(), None);
    }

    #[test]
    fn column_families_do_not_collide() {
        let store = MemoryStore::new();
        store.put("a", b"key", b"from-a").unwrap();
        store.put("b", b"key", b"from-b").unwrap();
        assert_eq!(store.get("a", b"key").unwrap(), Some(b"from-a".to_vec()));
        assert_eq!(store.get("b", b"key").unwrap(), Some(b"from-b".to_vec()));
    }

    #[test]
    fn iter_prefix_returns_matches_in_key_order() {
        let store = MemoryStore::new();
        store.put("events", b"evt:2", b"second").unwrap();
        store.put("events", b"evt:1", b"first").unwrap();
        store.put("events", b"other:1", b"excluded").unwrap();

        let matches = store.iter_prefix("events", b"evt:").unwrap();
        assert_eq!(
            matches,
            vec![
                (b"evt:1".to_vec(), b"first".to_vec()),
                (b"evt:2".to_vec(), b"second".to_vec())
            ]
        );
    }
}
