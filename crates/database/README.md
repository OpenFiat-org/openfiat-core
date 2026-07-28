# openfiat-database

The production `KvStore` implementation: `Database`, a RocksDB-backed store
opened with an explicit set of column families (`Database::open(path,
column_families)`). Every domain registry in this workspace names its own
column family (`advertisements`, `reservations`, `settlements`, ...); a real
node opens one `Database` naming all of them and shares it across every
registry, the same way tests share one `MemoryStore`.

## Depends on

- `openfiat-storage` — implements its `KvStore` trait.
- `openfiat-types` — shared value types.

## Used by

- `openfiat-cli` — the composition root opens the real `Database` here for
  a running node; every other crate's tests use `openfiat-storage`'s
  `MemoryStore` instead.
