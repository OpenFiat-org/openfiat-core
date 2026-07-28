# openfiat-oracles

External data oracle plugin architecture (OFS-7000): signed publications
(exchange rates, stablecoin metadata, payment infrastructure, regional
metadata) travel as gossip events, checked against `openfiat-registry`'s
on-file market-data providers. `median_exchange_rate` implements the
spec's one concrete aggregation method across every current (non-expired)
provider record for a pair. `provider` defines the local plugin interface
(`OracleProvider`) an external provider implementation fetches data
through before publishing; none ship in this crate.

**Spec:** OFS-7000 — OpenFiat Oracle Protocol

## Depends on

- `openfiat-registry` — checks a publisher is a registered market-data
  provider.
- `openfiat-gossip` — publications travel as gossip events.
- `openfiat-network`, `openfiat-types`, `openfiat-crypto`,
  `openfiat-serialization`, `openfiat-storage` — shared transport, types,
  signing, and storage.

## Used by

- `openfiat-rpc` — exposes oracle publish/lookup/median-rate over
  JSON-RPC.
