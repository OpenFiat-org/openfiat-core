# openfiat-advertisements

The "ads backend" (OFS-2100): merchant advertisement publication, Liquidity
Vault-backed availability tracking (`reserve_liquidity`/
`release_liquidity`), and automatic disabling when liquidity hits zero.
Every advertisement is signed and gossip-replicated; a node's local
registry is the source of truth `reservations` reads against when
validating a new reservation request.

**Spec:** OFS-2100 — OpenFiat Advertisement Protocol

## Depends on

- `openfiat-gossip` — advertisement events travel as gossip events.
- `openfiat-network`, `openfiat-types`, `openfiat-crypto`,
  `openfiat-serialization`, `openfiat-storage` — shared transport, types,
  signing, and storage.

## Used by

- `openfiat-reservations` — reserves/releases liquidity against a shared
  handle to this registry.
- `openfiat-rpc` — exposes advertisement publish/lookup over JSON-RPC.
- `openfiat-apps/explorer/indexer` (a separate repo) — joins advertisement
  data (asset/fiat currency/pricing) into its Trade view.

