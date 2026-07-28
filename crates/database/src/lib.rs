//! `openfiat-database` — RocksDB-backed implementation of `openfiat_storage::KvStore`.
//!
//! Every crate that needs persistence (the gossip event store, the Service
//! Registry, snapshot import/export, risk-intelligence indexing — see
//! `docs/architecture.md`) opens one [`Database`] over its own set of
//! column families and talks to it purely through the `KvStore` trait, so
//! its own tests can swap in `openfiat_storage::mem::MemoryStore` instead.

use openfiat_storage::{Entry, KvStore};
use openfiat_types::ErrorCode;
use rocksdb::{DB, IteratorMode, Options};
use std::fmt;
use std::path::Path;

/// A RocksDB-backed [`KvStore`].
pub struct Database {
    db: DB,
}

/// A RocksDB operation failed, or referenced a column family the database
/// wasn't opened with.
#[derive(Debug)]
pub enum DatabaseError {
    Rocks(rocksdb::Error),
    UnknownColumnFamily(String),
}

impl DatabaseError {
    /// The OFS-8000 code this failure maps to (`DATABASE_ERROR`, 9000).
    pub const fn code(&self) -> ErrorCode {
        ErrorCode::DatabaseError
    }
}

impl fmt::Display for DatabaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rocks(err) => write!(f, "RocksDB error: {err}"),
            Self::UnknownColumnFamily(cf) => write!(f, "unknown column family: {cf}"),
        }
    }
}

impl std::error::Error for DatabaseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Rocks(err) => Some(err),
            Self::UnknownColumnFamily(_) => None,
        }
    }
}

impl From<rocksdb::Error> for DatabaseError {
    fn from(err: rocksdb::Error) -> Self {
        Self::Rocks(err)
    }
}

impl Database {
    /// Open (creating if necessary) a database at `path` with exactly the
    /// given column families.
    pub fn open(path: impl AsRef<Path>, column_families: &[&str]) -> Result<Self, DatabaseError> {
        let mut options = Options::default();
        options.create_if_missing(true);
        options.create_missing_column_families(true);
        let db = DB::open_cf(&options, path, column_families)?;
        Ok(Self { db })
    }

    fn cf_handle(&self, cf: &str) -> Result<&rocksdb::ColumnFamily, DatabaseError> {
        self.db.cf_handle(cf).ok_or_else(|| DatabaseError::UnknownColumnFamily(cf.to_string()))
    }
}

impl KvStore for Database {
    type Error = DatabaseError;

    fn get(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, DatabaseError> {
        Ok(self.db.get_cf(self.cf_handle(cf)?, key)?)
    }

    fn put(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), DatabaseError> {
        Ok(self.db.put_cf(self.cf_handle(cf)?, key, value)?)
    }

    fn delete(&self, cf: &str, key: &[u8]) -> Result<(), DatabaseError> {
        Ok(self.db.delete_cf(self.cf_handle(cf)?, key)?)
    }

    fn iter_prefix(&self, cf: &str, prefix: &[u8]) -> Result<Vec<Entry>, DatabaseError> {
        let handle = self.cf_handle(cf)?;
        let mode = IteratorMode::From(prefix, rocksdb::Direction::Forward);
        let mut entries = Vec::new();
        for item in self.db.iterator_cf(handle, mode) {
            let (key, value) = item?;
            if !key.starts_with(prefix) {
                break;
            }
            entries.push((key.into_vec(), value.into_vec()));
        }
        Ok(entries)
    }
}

/// Crate version, re-exported for diagnostics and `openfiat-node --version`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn reports_a_version() {
        assert!(!version().is_empty());
    }

    #[test]
    fn round_trips_a_value() {
        let dir = tempdir().unwrap();
        let db = Database::open(dir.path(), &["events"]).unwrap();
        db.put("events", b"key", b"value").unwrap();
        assert_eq!(db.get("events", b"key").unwrap(), Some(b"value".to_vec()));
    }

    #[test]
    fn delete_removes_the_key() {
        let dir = tempdir().unwrap();
        let db = Database::open(dir.path(), &["events"]).unwrap();
        db.put("events", b"key", b"value").unwrap();
        db.delete("events", b"key").unwrap();
        assert_eq!(db.get("events", b"key").unwrap(), None);
    }

    #[test]
    fn column_families_do_not_collide() {
        let dir = tempdir().unwrap();
        let db = Database::open(dir.path(), &["a", "b"]).unwrap();
        db.put("a", b"key", b"from-a").unwrap();
        db.put("b", b"key", b"from-b").unwrap();
        assert_eq!(db.get("a", b"key").unwrap(), Some(b"from-a".to_vec()));
        assert_eq!(db.get("b", b"key").unwrap(), Some(b"from-b".to_vec()));
    }

    #[test]
    fn iter_prefix_returns_matches_in_key_order() {
        let dir = tempdir().unwrap();
        let db = Database::open(dir.path(), &["events"]).unwrap();
        db.put("events", b"evt:2", b"second").unwrap();
        db.put("events", b"evt:1", b"first").unwrap();
        db.put("events", b"other:1", b"excluded").unwrap();

        let matches = db.iter_prefix("events", b"evt:").unwrap();
        assert_eq!(matches, vec![(b"evt:1".to_vec(), b"first".to_vec()), (b"evt:2".to_vec(), b"second".to_vec())]);
    }

    #[test]
    fn unopened_column_family_is_reported_not_panicked() {
        let dir = tempdir().unwrap();
        let db = Database::open(dir.path(), &["events"]).unwrap();
        let err = db.get("nonexistent", b"key").unwrap_err();
        assert_eq!(err.code(), ErrorCode::DatabaseError);
    }

    #[test]
    fn reopening_the_same_path_persists_data() {
        let dir = tempdir().unwrap();
        {
            let db = Database::open(dir.path(), &["events"]).unwrap();
            db.put("events", b"key", b"value").unwrap();
        }
        let db = Database::open(dir.path(), &["events"]).unwrap();
        assert_eq!(db.get("events", b"key").unwrap(), Some(b"value".to_vec()));
    }
}
