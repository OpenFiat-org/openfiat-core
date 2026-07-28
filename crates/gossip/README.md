# openfiat-gossip

The event propagation backbone (OFS-1200): origination, role-scoped
origination authorization (`authorization::is_authorized`), duplicate
suppression, TTL-bounded forwarding, and catch-up recovery on reconnect.
`GossipService` is the hub every domain crate wraps: `originate(...)` signs
and broadcasts a new event, `add_event_handler(...)` registers a callback
invoked for every event this node stores (self-originated or received).

A single `GossipService` supports multiple registered handlers (appends,
never replaces) — this is what lets a real node multiplex every domain's
events through one shared gossip channel instead of running one connection
per domain.

**Spec:** OFS-1200 — OpenFiat Gossip Protocol

## Depends on

- `openfiat-network` — the transport gossip messages travel over.
- `openfiat-types`, `openfiat-crypto`, `openfiat-serialization`,
  `openfiat-storage` — shared types, signing, and the event-dedup store.

## Used by

Every domain crate that replicates state via signed events: `snapshot`,
`sessions`, `registry`, `identity`, `trade` (indirectly, via reservations/
settlement), `advertisements`, `reservations`, `settlement`, `disputes`,
`governance`, `notifications`, `oracles`, `risk`.
