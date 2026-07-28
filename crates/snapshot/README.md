# openfiat-snapshot

Fast-sync for new/recovering nodes (OFS-1300): signed snapshot
announcements travel as gossip events (metadata only — actual state bytes
travel over whatever transport a client chooses, out of this crate's
scope), and `SnapshotIndex::import` verifies protocol compatibility,
downloaded size, and a SHA-256 state-root match before advancing this
node's local checkpoint height. Deliberately never looks inside a
snapshot's state bytes — what's actually indexed is every other domain
crate's concern.

Snapshot providers register through `openfiat-registry` directly
(`ServiceType::Infrastructure(SnapshotProvider)`) rather than a separate
registration event.

**Spec:** OFS-1300 — Snapshot Synchronization Protocol

## Depends on

- `openfiat-gossip` — announcements travel as gossip events.
- `openfiat-registry` — checks the announcer is a registered provider.
- `openfiat-network`, `openfiat-types`, `openfiat-crypto`,
  `openfiat-serialization`, `openfiat-storage` — shared transport, types,
  signing, and storage.

## Used by

- `openfiat-rpc` — exposes snapshot discovery/announcement over JSON-RPC.
