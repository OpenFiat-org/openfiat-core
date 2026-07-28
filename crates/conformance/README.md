# openfiat-conformance

Phase 10's multi-node integration harness — not part of the shipped node
(`publish = false`, no dependency on it from `openfiat-cli` or any real
service). It exists so whole-stack behavior can be tested against a real
libp2p cluster instead of re-deriving it from reading 20+ crates' worth of
per-domain unit tests.

`FullNode<S>` composes every domain registry this workspace has built —
the same shared-store composition `openfiat-rpc`'s `NodeState` uses — but,
unlike `NodeState`, actually wires each registry's `apply_event` onto one
real, gossip-connected `GossipService`. A mutation on one node in a
cluster genuinely propagates to the rest, the way a real node behaves and
`NodeState` deliberately doesn't need to (see that crate's README).

`spawn_cluster` brings up `n` such nodes on real QUIC/loopback sockets,
dialed into a hub and mutually peer-key-registered, converged to fully
connected — the bootstrap sequence every `tests/replication.rs` in this
workspace already hand-rolls per file, centralized here.

## What lives here vs. per-crate tests

Each domain crate's own `tests/replication.rs` proves that domain's
events converge in isolation, with a bare `GossipService<S>` and nothing
else attached. This crate's own tests exist only where composing
*everything onto one node* is itself the thing under test:

- `tests/trade_lifecycle.rs` — a real advertisement → reservation →
  settlement → trade chain across three domain crates, something no
  single crate's own tests exercise end to end.
- `tests/partition_recovery.rs` — a composed node (all domains, one
  gossip channel) drops offline, misses events across two unrelated
  domains at once, reconnects, and recovers both — proving the
  composition itself doesn't break gossip's own eventual-consistency
  guarantee.

See `/CONFORMANCE.md` at the repository root for the full mapping from
each spec's conformance checklist to the test (here or in a per-domain
crate) that verifies it.

## Depends on

Every domain crate composed into `FullNode` — `openfiat-advertisements`,
`openfiat-reservations`, `openfiat-settlement`, `openfiat-trade`,
`openfiat-disputes`, `openfiat-identity`, `openfiat-reputation`,
`openfiat-governance`, `openfiat-registry`, `openfiat-notifications`,
`openfiat-oracles`, `openfiat-risk`, `openfiat-snapshot`,
`openfiat-sessions` — plus `openfiat-gossip`, `openfiat-network`,
`openfiat-types`, `openfiat-crypto`, `openfiat-serialization`, and
`openfiat-storage` underneath all of them.

## Used by

Nothing — this is the leaf of the dependency graph, exercised only by its
own `tests/` and its own `src/lib.rs` unit test.
