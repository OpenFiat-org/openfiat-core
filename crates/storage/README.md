# openfiat-storage

The `KvStore` trait every persistence-needing crate programs against:
column-family-scoped get/put/delete/prefix-iteration. Ships one real (not
mocked) implementation, `mem::MemoryStore`, used throughout this workspace's
own test suites instead of standing up RocksDB per test. `openfiat-database`
provides the production RocksDB-backed implementation of the same trait.

Also provides a `KvStore` impl for `Rc<T>`, so one physical store can back
several domain registries at once — each still takes its `S` by value, but
`Rc::clone` is cheap and every registry writes to its own column family.

## Depends on

Nothing in this workspace — a pure abstraction.

## Used by

Every crate with a replicated registry: `database`, `discovery`, `gossip`,
`snapshot`, `sessions`, `registry`, `identity`, `reputation`, `trade`,
`advertisements`, `reservations`, `settlement`, `disputes`, `governance`,
`notifications`, `oracles`, `risk`, `rpc`, `api`.
