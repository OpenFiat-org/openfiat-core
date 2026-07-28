# openfiat-serialization

Canonical wire/storage encoding (`wire::to_bytes`/`wire::from_bytes`, backed
by `postcard`) — compact, deterministic, `no_std`-friendly. Chosen over
`bincode` because bincode's own maintainers flag it unmaintained with no
safe upgrade path (`cargo deny` catches this as an advisory failure). Every
signed event, registry record, and gossip payload in this workspace is
encoded this way; JSON stays at the HTTP/RPC boundary (`rpc`, `api`) where
human/cross-language readability matters more than size.

## Depends on

- `openfiat-types` — encodes/decodes the shared protocol types.

## Used by

Nearly every crate that signs an event or persists a record: `network`,
`discovery`, `gossip`, `snapshot`, `sessions`, `registry`, `identity`,
`wallet`, `advertisements`, `reservations`, `settlement`, `disputes`,
`governance`, `notifications`, `oracles`, `risk`, `rpc`.
