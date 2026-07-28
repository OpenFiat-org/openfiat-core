# openfiat-serialization

Two encodings, two boundaries. `wire::to_bytes`/`wire::from_bytes`
(`postcard`) — compact, deterministic, `no_std`-friendly — for gossip
transport and RocksDB storage values, chosen over `bincode` because
bincode's own maintainers flag it unmaintained with no safe upgrade path
(`cargo deny` catches this as an advisory failure). `json::to_bytes`/
`json::from_bytes` (`serde_json`) for every signed event's own signature
bytes and the RPC boundary's `sendX` payload (`rpc`, `api`) — a signature
a non-Rust SDK can reproduce with any language's stdlib JSON encoder,
rather than needing to reimplement postcard's binary encoding rules.

## Depends on

- `openfiat-types` — encodes/decodes the shared protocol types.

## Used by

Nearly every crate that signs an event or persists a record: `network`,
`discovery`, `gossip`, `snapshot`, `sessions`, `registry`, `identity`,
`wallet`, `advertisements`, `reservations`, `settlement`, `disputes`,
`governance`, `notifications`, `oracles`, `risk`, `rpc`.
