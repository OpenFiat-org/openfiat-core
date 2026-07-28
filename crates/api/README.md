# openfiat-api

The "swagger-like documentation" layer over `openfiat-rpc`'s JSON-RPC
surface. `GET /openrpc.json` serves an OpenRPC 1.2.6 document (the
JSON-RPC equivalent of an OpenAPI/Swagger spec) generated directly from
`openfiat-rpc`'s own live dispatch table, so the method list can never
drift from what the node actually runs. `GET /docs` serves a small,
self-contained interactive reference page — list every method, see its
shape, run it live against `/rpc` on the same origin.

A static, always-current mirror of this same reference is also published
in [`openfiat-docs`](https://github.com/OpenFiat-org/openfiat-docs) for
browsing without a running node; this crate is the live, per-node version.

## Depends on

- `openfiat-rpc` — the dispatch table `openrpc.json` is generated from.
- `openfiat-storage` — the `KvStore` bound used to build a table instance
  for spec generation.

## Used by

- `openfiat-cli` — the composition root serves this alongside `rpc`.
