# openfiat-reservations

Trade reservation and escrow locking (OFS-2200): validates a reservation
request against a shared handle to `openfiat-advertisements`' registry
(enough liquidity, ad still active), locks that liquidity, and enforces the
30-minute validation window. This crate's own authority ends at
`EscrowLocked` — everything after that is `openfiat-settlement`'s.

**Spec:** OFS-2200 — OpenFiat Reservation Protocol

## Depends on

- `openfiat-advertisements` — shares its registry to reserve/release
  liquidity against a live advertisement.
- `openfiat-gossip` — reservation events travel as gossip events.
- `openfiat-network`, `openfiat-types`, `openfiat-crypto`,
  `openfiat-serialization`, `openfiat-storage` — shared transport, types,
  signing, and storage.

## Used by

- `openfiat-trade`, `openfiat-reputation` — join reservation data into a
  wider view (trade status, reputation metrics).
- `openfiat-rpc` — exposes reservation request/lookup over JSON-RPC.

`openfiat-settlement` references a `ReservationId` but doesn't depend on
this crate directly; `openfiat-disputes` only reaches reservation data
through `openfiat-settlement`'s own copy of it.
