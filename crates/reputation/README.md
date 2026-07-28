# openfiat-reputation

Behavioral reputation scoring (OFS-3000): trade success rate, dispute rate,
lifetime volume, average ticket size, settlement speed, merchant age, and
a placeholder merchant-tier ladder — all computed on demand, purely by
reading `reservations`/`settlement`/`disputes`' already-replicated
registries. Deliberately has **no signed event type or store of its own**:
a wallet-signed "I completed a trade" claim about itself is exactly the
kind of self-asserted signal OFS-3000's anti-manipulation goals rule out,
and every node already converges on identical settlement/dispute state
independently, so recomputing reputation from that state gives identical
results everywhere for free.

**Spec:** OFS-3000 — OpenFiat Reputation Engine

## Depends on

- `openfiat-reservations`, `openfiat-settlement`, `openfiat-disputes` —
  the registries this crate's view reads, never mutates.
- `openfiat-types`, `openfiat-storage` — shared types and the `KvStore`
  bound its view is generic over.

## Used by

- `openfiat-swqos` — (planned) weights traffic priority by reputation.
- `openfiat-rpc` — exposes `getReputation` over JSON-RPC.
