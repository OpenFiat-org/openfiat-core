# openfiat-cli

The `openfiat-node` binary — the composition root wiring every crate above
into one running node. Currently prints each linked crate's version only;
real startup (config loading, opening the RocksDB `Database`, wiring
`GossipService` to every domain registry the way `openfiat-apps/explorer/
indexer` already does, serving `rpc`+`api`) is Phase 12 territory in the
workspace's implementation plan.

## Depends on

- `openfiat-config` — loads a node's configuration.
- `openfiat-database` — opens the real RocksDB-backed store.
- `openfiat-network` — the P2P transport.
- `openfiat-rpc`, `openfiat-api` — the JSON-RPC + documentation surface
  this binary serves.
- `openfiat-metrics` — node-level telemetry.

## Used by

Nothing — this is the top-level binary.
