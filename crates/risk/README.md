# openfiat-risk

Risk intelligence provider plugin architecture (OFS-7100): signed risk
records (Flagged/Cleared, with a severity tier) travel as gossip events,
checked against `openfiat-registry`'s on-file risk providers. Wallet
screening aggregates to the worst severity among current, unsuperseded
Flagged records — a later Cleared record supersedes every Flagged record
published before it. `provider` defines the local plugin interface
(`RiskProvider`) an external adapter (e.g. for a blockchain analytics
company) queries before publishing a record; none ship in this crate.

**Spec:** OFS-7100 — OpenFiat Risk Intelligence Protocol

## Depends on

- `openfiat-registry` — checks a publisher is a registered risk
  intelligence provider.
- `openfiat-gossip` — risk records travel as gossip events.
- `openfiat-network`, `openfiat-types`, `openfiat-crypto`,
  `openfiat-serialization`, `openfiat-storage` — shared transport, types,
  signing, and storage.

## Used by

- `openfiat-rpc` — exposes risk publish/wallet-screening over JSON-RPC.
