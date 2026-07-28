# openfiat-rpc

The third-party-facing JSON-RPC 2.0 surface — modeled directly on Solana's
own JSON-RPC API: one `POST /rpc` endpoint, `getX`/`sendX` camelCase
method names, `sendX` methods taking an opaque, already-signed base64
payload the caller's own wallet produced (mirroring `sendTransaction` —
this crate never constructs or signs anything on the caller's behalf).
`GET /ws` streams a generic firehose of successful mutations.

`NodeState` composes every domain registry this workspace has built.
Since that state is `Rc`-based (not `Send`), it lives entirely inside one
dedicated OS thread running its own single-threaded Tokio runtime; axum
handlers hold `RpcHandle` — a plain `Send + Sync` channel — and talk to
that thread over a channel (see the `actor` module).

## Depends on

- `openfiat-advertisements`, `openfiat-reservations`, `openfiat-settlement`,
  `openfiat-trade`, `openfiat-disputes`, `openfiat-identity`,
  `openfiat-reputation`, `openfiat-governance`, `openfiat-registry`,
  `openfiat-notifications`, `openfiat-oracles`, `openfiat-risk`,
  `openfiat-snapshot`, `openfiat-sessions` — every domain this node's
  method table dispatches into.
- `openfiat-metrics` — request counters exposed at `GET /metrics`.
- `openfiat-network`, `openfiat-types`, `openfiat-crypto`,
  `openfiat-serialization`, `openfiat-storage` — shared transport, types,
  signing, and the `KvStore` bound `NodeState` is generic over.

## Used by

- `openfiat-api` — merges its own OpenRPC/reference-docs router onto this
  crate's router into one axum app.
- `openfiat-cli` — the composition root serves this alongside `api`.
