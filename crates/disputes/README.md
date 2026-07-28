# openfiat-disputes

Decentralized commit-reveal arbitration (OFS-2400): a dispute opens against
a settlement (buyer/seller identity copied from `openfiat-settlement`'s
already-verified record), arbitrators join up to a required threshold, then
commit and reveal votes, resolving by majority. Real OPEN staking and
slashing are Solana program operations this layer doesn't invoke yet — the
same deferral `openfiat-settlement` makes for escrow release.

**Spec:** OFS-2400 — OpenFiat Dispute Protocol

## Depends on

- `openfiat-settlement` — shares its registry to validate a dispute
  opener is actually a party, and to read buyer/seller identity.
- `openfiat-gossip` — dispute events travel as gossip events.
- `openfiat-network`, `openfiat-types`, `openfiat-crypto`,
  `openfiat-serialization`, `openfiat-storage` — shared transport, types,
  signing, and storage.

## Used by

- `openfiat-reputation` — dispute outcomes feed a wallet's dispute-rate
  and disputes-lost metrics.
- `openfiat-rpc` — exposes dispute open/commit/reveal over JSON-RPC.
