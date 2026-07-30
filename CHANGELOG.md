# Changelog

All notable changes to `openfiat-core` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
  replaying all history. Configured with `--snapshot-public-url`
  (required to produce; a node consumes snapshots with no configuration),
  `--snapshot-dir`, and `--snapshot-interval-secs`.
- Initial repository scaffold: directory layout, CI, developer tooling,
  and community health files.

### Changed

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
