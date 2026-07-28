# openfiat-sessions

Signed session lifecycle over gossip (OFS-1400): establish, renew, revoke,
and migrate a temporary client authorization without a centralized session
database. A wallet may hold several concurrent sessions (desktop, mobile,
merchant terminal, API client) — each is its own record; revoking one never
affects the others. Renewal/migration require a strictly increasing version
number; revocation is permanent.

**Spec:** OFS-1400 — Session Synchronization Protocol

## Depends on

- `openfiat-gossip` — session events travel as gossip events.
- `openfiat-network`, `openfiat-types`, `openfiat-crypto`,
  `openfiat-serialization`, `openfiat-storage` — shared transport, types,
  signing, and storage.

## Used by

Nothing yet — a real RPC signed-request auth flow that establishes a
session per connected client is a natural follow-up once `rpc`'s
per-request auth model needs one; today `rpc` authenticates each mutation
by its own embedded signature instead.
