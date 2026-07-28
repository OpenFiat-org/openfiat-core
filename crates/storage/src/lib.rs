//! `openfiat-storage` — Storage engine abstraction: column families, get/put/delete, iteration.
//!
//! This crate defines [`KvStore`], the trait every persistence-needing
//! crate programs against, and [`mem::MemoryStore`], a real (not mocked)
//! in-memory implementation of it. Programming against the trait — rather
//! than `openfiat-database`'s RocksDB type directly — lets every other
//! crate's tests use [`mem::MemoryStore`] instead of standing up a real
//! RocksDB instance per test.

pub mod mem;

/// A `(key, value)` pair as returned by [`KvStore::iter_prefix`].
pub type Entry = (Vec<u8>, Vec<u8>);

/// A key-value store organized into named column families.
///
/// A column family is just a logical namespace: `("events", key)` and
/// `("registry", key)` never collide even if `key` is identical.
pub trait KvStore {
    type Error: std::error::Error;

    /// The value stored at `key` in `cf`, or `None` if absent.
    fn get(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error>;

    /// Write `value` at `key` in `cf`, overwriting any existing value.
    fn put(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), Self::Error>;

    /// Remove `key` from `cf`, if present.
    fn delete(&self, cf: &str, key: &[u8]) -> Result<(), Self::Error>;

    /// All `(key, value)` pairs in `cf` whose key starts with `prefix`, in
    /// ascending key order.
    fn iter_prefix(&self, cf: &str, prefix: &[u8]) -> Result<Vec<Entry>, Self::Error>;
}

/// Crate version, re-exported for diagnostics and `openfiat-node --version`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_a_version() {
        assert!(!version().is_empty());
    }
}
