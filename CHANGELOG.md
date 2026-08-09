# Changelog

All notable changes to `openfiat-core` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Security

- **BREAKING — domain-separated signing preimages for all client-signed wire
  events (F-01).** Every client-signed marketplace event (advertisements,
  reservations, registrations, settlements, sessions, disputes, votes,
  proposals, claims, reviews, oracle/risk publishes, trade-channel entries,
  notifications, …) is now signed over `domain::preimage(tag, payload)` —
  `len(tag):u32be ‖ tag ‖ json(payload)` with a per-type `/v1` tag — instead of
  the bare `json(payload)`. This binds the payload's *type* into the signature,
  closing a class of signature-replay collisions where two events with the same
  field shape (e.g. a claim-verify vs a claim-revoke) shared a preimage and one
  signature validated for the other. A node on this version **rejects
  signatures produced by any pre-domain-separation signer**, and older nodes
  reject this version's signatures — so **all clients (Rust SDK, TypeScript
  SDK, app) must upgrade in lockstep** (hard cutover; no dual-accept window).
  The cross-language byte layout is frozen by
  `crates/serialization/tests/vectors/client_signed_v1.json`. Also fixes a
  latent bug where governance `withdraw_proposal`/`activate_proposal`'s
  direct-sign path signed untagged bytes its verifier rejected.

### Added

- Node logging. `openfiat-node` emits structured `tracing` output, filtered
  with `--log <FILTER>` (default `info`, per-module directives accepted:
  `--log info,openfiat_rpc::actor=debug`). Dependencies are held at `warn`
  so the node's own lines are not buried. Previously the binary printed six
  lines at startup and nothing afterwards, so a node that had stopped doing
  useful work was indistinguishable from a healthy one.
- Snapshot download (OFS-1300 §14). A node now serializes its own state on
  a configured interval, writes it to `--snapshot-dir`, serves it at
  `GET /snapshot/{id}` on its existing HTTP port, and — when it has no
  checkpoint — bootstraps itself from an announced snapshot instead of
  replaying all history. Configured with `--snapshot-dir` and
  `--snapshot-interval-secs`; neither is required, and a node consumes
  snapshots with no configuration at all.
- Initial repository scaffold: directory layout, CI, developer tooling,
  and community health files.

### Changed

- **Snapshot production is on by default and derives its own location.**
  A node works out where peers should fetch its snapshots from — the
  addresses it has learned it is reachable at (libp2p listen addresses per
  interface, and identify's `observed_addr`, which is the only thing that
  sees through NAT) plus its `--rpc-bind-address` port, which already
  serves `GET /snapshot/{id}`. Globally reachable hosts are announced
  ahead of private ones, and a fetching node tries them in that order.

  `--snapshot-public-url` survives as an override for a node whose HTTP
  server is reached on a hostname or port it cannot observe — a reverse
  proxy — and `--public-rpc-url`, if set, serves as that override by
  default, being the same fact about the same server. `--no-snapshot-production`
  turns production off for an operator who cannot spare the disk or the
  read. Previously the flag *gated* the feature: omitting it disabled
  snapshot production silently, so the common node contributed nothing to
  anyone else's bootstrap. Location is no longer configuration; frequency
  still is.
- Bootstrapping tries every snapshot it has verified, highest height
  first, rather than only the highest. A single unreachable or corrupt
  producer at the top of the list used to stall a joining node
  indefinitely — it re-asked the same dead host every thirty seconds —
  while a usable snapshot one height down went unfetched.
- The first snapshot is written one interval after startup rather than
  immediately, so it describes state the node has accumulated or imported
  rather than the empty store it booted with.
- **Breaking: node configuration is command-line flags only.** Every
  `CLI_*` environment variable is gone, replaced by the flags documented in
  the README's "Running a node": `--ledger`, `--identity`,
  `--rpc-bind-address`, `--gossip-bind-address`, `--entrypoint` (repeatable),
  `--solana-rpc-url` (repeatable), `--snapshot-dir`,
  `--snapshot-public-url`, `--snapshot-interval-secs`, `--log`. Environment
  configuration made a node's behaviour depend on ambient state that
  `ps`, a service file, and a bug report all fail to show.
- **Breaking (devnet): `SnapshotAnnounce` payload.** `SnapshotMetadata`
  gained a `locations` field naming where a snapshot can be downloaded,
  inside the signed payload, so **every announcement signed before this
  change fails verification**. `SUPPORTED_PROTOCOL_VERSION` is bumped
  1 → 2 so such records are rejected as a version mismatch rather than as
  a bad signature. Nothing importable is lost: an announcement without a
  location was never fetchable. Producers must re-announce.

### Fixed

- A `--solana-rpc-url` pointing at a non-JSON-RPC endpoint killed the chain
  thread. `solana-client` reads a response as `value["result"]`, which
  panics on the JSON array Helius's Enhanced Transactions REST path
  (`/v0/transactions/`) returns; the HTTP server survived, so the node
  reported itself healthy while doing nothing on chain. Each endpoint is now
  probed with `getVersion` at startup and the node refuses to run with a
  message naming the URL, rather than dying later.
- A node's RocksDB open list omitted `gossip_events`, so every gossip
  event write on a RocksDB-backed node failed silently against a column
  family that was never opened. The list is now derived from the single
  definition of what makes up a node's state.

[Unreleased]: https://github.com/OpenFiat-org/openfiat-core/commits/main
