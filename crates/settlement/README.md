# openfiat-settlement

Settlement coordination (OFS-2300): the state machine from `EscrowLocked`
onward (awaiting payment → payment submitted → approved → completed, or
rejected/cancelled/disputed), referencing a `openfiat-reservations`
reservation by ID. Actual on-chain escrow release ("escrow release is
performed exclusively by the OpenFiat Program") is a Solana instruction
this P2P coordination layer doesn't invoke yet — `Approved` transitioning
straight to `Completed` here models this crate's own authority ending
where the on-chain program's begins.

**Spec:** OFS-2300 — OpenFiat Settlement Protocol

## Depends on

- `openfiat-reservations` — a settlement always references a reservation
  by `ReservationId` (not a shared registry handle — settlement doesn't
  validate against reservation state itself).
- `openfiat-gossip` — settlement events travel as gossip events.
- `openfiat-network`, `openfiat-types`, `openfiat-crypto`,
  `openfiat-serialization`, `openfiat-storage` — shared transport, types,
  signing, and storage.

## Used by

- `openfiat-disputes` — shares its registry to validate a dispute opener
  is actually a party to the settlement, and copies buyer/seller identity.
- `openfiat-trade`, `openfiat-reputation` — join settlement data into a
  wider view (trade status, reputation metrics).
- `openfiat-rpc` — exposes settlement initiate/payment/approve over
  JSON-RPC.
