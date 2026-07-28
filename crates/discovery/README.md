# openfiat-discovery

Peer discovery (OFS-1100): bootstrap node connection, routing table, peer
exchange, converging a cluster of nodes started from one bootstrap node to
a consistent peer set.

**Spec:** OFS-1100 — OpenFiat Node Synchronization Protocol (peer exchange)

## Depends on

- `openfiat-network` — the transport peers connect over.
- `openfiat-types`, `openfiat-crypto`, `openfiat-serialization`,
  `openfiat-storage` — shared types, signing, and a peer-cache store.

## Used by

Nothing yet — every domain crate's own tests dial peers directly rather
than going through discovery's peer exchange. Wiring real peer-key
learning through discovery (rather than each service's own hardcoded
`known_peer_keys`/`register_peer_key` calls) is a natural next step once a
real multi-node devnet composition (`cli`) needs it.
