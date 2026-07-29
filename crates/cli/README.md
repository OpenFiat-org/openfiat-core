# openfiat-cli

The `openfiat-node` binary — the composition root wiring every crate above
into one running node: a real RocksDB-backed store (`CLI_DATA_DIR`), a
Solana wallet.json node identity (`CLI_WALLET_PATH`), and the merged
`rpc`+`api` axum server bound to a real HTTP port (`CLI_HTTP_ADDR`).

```sh
CLI_DATA_DIR=./data CLI_HTTP_ADDR=127.0.0.1:7080 cargo run -p openfiat-cli
```

serves `POST /rpc`, `GET /ws`, `GET /health`, `GET /metrics` (from
`openfiat-rpc`) and `GET /openrpc.json`, `GET /docs` (from `openfiat-api`)
— the exact surface OFS-8200 describes.

This binary does not yet run real libp2p/gossip networking: `openfiat-rpc`'s
`sendX` handlers apply a caller's signed payload straight to the local
registry (see `crates/rpc/src/state.rs`'s own doc comment) rather than
originating it over `openfiat-gossip` for other nodes to pick up. That's an
intentional, already-documented scope boundary of the RPC layer, not
something new — multi-node propagation of RPC-submitted writes is a real,
separately-scoped follow-up.

## Depends on

- `openfiat-database` — opens the real RocksDB-backed store.
- `openfiat-wallet` — loads the node's Solana wallet.json identity.
- `openfiat-rpc`, `openfiat-api` — the JSON-RPC + documentation surface
  this binary serves.
- `openfiat-metrics` — node-level telemetry.

## Used by

Nothing — this is the top-level binary.
