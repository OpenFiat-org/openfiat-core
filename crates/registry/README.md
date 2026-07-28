# openfiat-registry

The decentralized Service Registry (OFS-1500): registration, health-state
updates (Online/Maintenance/Degraded/Offline), and auto-expiration on
missed health updates, replicated the same way as everything else —
signed events over gossip. `ServiceType` (from `openfiat-types`) already
covers every provider category this workspace has built
(`Infrastructure(SnapshotProvider)`, `MarketData(...)`,
`Security(RiskIntelligenceProvider)`, `Notifications(...)`), so every
provider crate registers through here directly rather than defining its
own registration event.

**Spec:** OFS-1500 — Service Registry Protocol

## Depends on

- `openfiat-gossip` — registrations/updates travel as gossip events.
- `openfiat-network`, `openfiat-types`, `openfiat-crypto`,
  `openfiat-serialization`, `openfiat-storage` — shared transport, types,
  signing, and storage.

## Used by

- `openfiat-snapshot`, `openfiat-notifications`, `openfiat-oracles`,
  `openfiat-risk` — each checks a publisher/provider is registered here
  before accepting its signed records.
- `openfiat-rpc` — exposes provider discovery over JSON-RPC.
