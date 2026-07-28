# openfiat-governance

Protocol governance (OFS-4000): proposals open directly for voting at
creation (discussion/technical review happen off-protocol, not as a
separate gossip-visible state), votes are cast and tallied, and a
proposal resolves to Accepted/Rejected once its voting window closes —
computed locally by every node from already-replicated votes, the same
deterministic-derivation approach `openfiat-disputes`' consensus uses.
Real voting-power computation from OPEN token balance/stake is a future
integration this layer doesn't have yet.

**Spec:** OFS-4000 — OpenFiat Governance Protocol

## Depends on

- `openfiat-gossip` — proposal/vote events travel as gossip events.
- `openfiat-network`, `openfiat-types`, `openfiat-crypto`,
  `openfiat-serialization`, `openfiat-storage` — shared transport, types,
  signing, and storage.

## Used by

- `openfiat-rpc` — exposes proposal create/vote/lookup over JSON-RPC.
- `openfiat-apps/explorer/indexer` (a separate repo) — the source for the
  explorer's Proposal/Governance view.
