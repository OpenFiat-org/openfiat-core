# openfiat-notifications

Notification gateway plugin architecture (OFS-6000): wallet subscription
preferences and provider delivery reports travel as signed gossip events.
Providers register through `openfiat-registry` directly
(`ServiceType::Notifications`) rather than a separate registration event —
a delivery report is only accepted from whichever peer the registry has on
file as that service's provider. `provider` defines the local plugin
interface (`NotificationProvider`) a concrete channel adapter (email, SMS,
Telegram, ...) implements; none ship in this crate.

**Spec:** OFS-6000 — OpenFiat Notification Protocol

## Depends on

- `openfiat-registry` — checks a delivery report's signer is the
  registered provider for the referenced service.
- `openfiat-gossip` — subscription/delivery events travel as gossip
  events.
- `openfiat-network`, `openfiat-types`, `openfiat-crypto`,
  `openfiat-serialization`, `openfiat-storage` — shared transport, types,
  signing, and storage.

## Used by

- `openfiat-rpc` — exposes subscription/delivery-report operations over
  JSON-RPC.
