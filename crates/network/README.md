# openfiat-network

The P2P transport layer (OFS-1000): libp2p (Noise + QUIC + Yamux) node
identity, connection lifecycle, and the message envelope. `identity::
peer_id_from_public_key` is the canonical PeerId derivation every other
crate relies on for the "does this signature's claimed identity actually
match its embedded public key" self-consistency check used throughout this
workspace.

**Spec:** OFS-1000 — OpenFiat Network Protocol

## Depends on

- `openfiat-types`, `openfiat-serialization`, `openfiat-crypto` — shared
  types, wire encoding, and the keypair this node's identity is built from.

## Used by

Every crate that runs its own `GossipService`/`Node`: `discovery`,
`gossip`, `snapshot`, `sessions`, `registry`, `swqos`, `identity`, `wallet`,
`reputation`, `trade`, `advertisements`, `reservations`, `settlement`,
`disputes`, `governance`, `notifications`, `oracles`, `risk`, `rpc`, `cli`.
