# openfiat-trade

The trade-lifecycle orchestrator (OFS-2000) — but with no state machine,
signed events, or gossip origination of its own. A `Trade` is purely a
read-time join of a `Reservation` (owned by `openfiat-reservations`) and
its `Settlement`, if one has started (owned by `openfiat-settlement`),
correlated by `ReservationId`, with one aggregate `TradeStatus` computed
from both.

**Spec:** OFS-2000 — OpenFiat Fiat Trading Protocol (orchestration only;
the two sub-protocols it composes each own their own state machine)

## Depends on

- `openfiat-reservations`, `openfiat-settlement` — the two registries this
  crate's view joins, never mutates.
- `openfiat-types`, `openfiat-storage` — shared types and the `KvStore`
  bound its view is generic over.

## Used by

- `openfiat-rpc` — exposes `getTrade`/`getTrades` over JSON-RPC.
- `openfiat-apps/explorer/indexer` (a separate repo) — the primary source
  for the explorer's Trade view.
