# openfiat-types

Shared protocol vocabulary: `PeerId`/`PublicKey`/`Signature` (node identity),
`Amount` (fixed-point money, never a float), `Timestamp`, `ServiceId`/
`ServiceType`, the gossip `EventEnvelope`/`EventType`/`EventId`, `Priority`,
and the canonical `ErrorCode` registry (OFS-8000). No logic beyond basic
construction/validation — this crate exists so every other crate references
the same concrete types instead of each inventing its own.

## Depends on

Nothing in this workspace — the foundation everything else builds on.

## Used by

Every other crate in this workspace, directly or transitively. If you're
looking for where a shared type is defined, it's here.
