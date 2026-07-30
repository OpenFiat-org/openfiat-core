# Changelog

All notable changes to `openfiat-core` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Snapshot download (OFS-1300 §14). A node now serializes its own state on
  a configured interval, writes it to `CLI_SNAPSHOT_DIR`, serves it at
  `GET /snapshot/{id}` on its existing HTTP port, and — when it has no
  checkpoint — bootstraps itself from an announced snapshot instead of
  replaying all history. Configured with `CLI_SNAPSHOT_PUBLIC_URLS`
  (required to produce; a node consumes snapshots with no configuration),
  `CLI_SNAPSHOT_DIR`, and `CLI_SNAPSHOT_INTERVAL_SECS`.
- Initial repository scaffold: directory layout, CI, developer tooling,
  and community health files.

### Changed

- **Breaking (devnet): `SnapshotAnnounce` payload.** `SnapshotMetadata`
  gained a `locations` field naming where a snapshot can be downloaded,
  inside the signed payload, so **every announcement signed before this
  change fails verification**. `SUPPORTED_PROTOCOL_VERSION` is bumped
  1 → 2 so such records are rejected as a version mismatch rather than as
  a bad signature. Nothing importable is lost: an announcement without a
  location was never fetchable. Producers must re-announce.

### Fixed

- A node's RocksDB open list omitted `gossip_events`, so every gossip
  event write on a RocksDB-backed node failed silently against a column
  family that was never opened. The list is now derived from the single
  definition of what makes up a node's state.

[Unreleased]: https://github.com/OpenFiat-org/openfiat-core/commits/main
