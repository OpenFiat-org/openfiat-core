<div align="center">

# openfiat-core

**Reference implementation of the OpenFiat protocol node, written in Rust.**

[![CI](https://github.com/OpenFiat-org/openfiat-core/actions/workflows/ci.yml/badge.svg)](https://github.com/OpenFiat-org/openfiat-core/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](LICENSE)
[![Discussions](https://img.shields.io/github/discussions/OpenFiat-org/openfiat-core)](https://github.com/orgs/OpenFiat-org/discussions)

[Website](https://openfiat.network) · [Docs](https://docs.openfiat.network) · [Specs](https://github.com/OpenFiat-org/openfiat-specs) · [Contributing](CONTRIBUTING.md)

</div>

---

## About

`openfiat-core` is part of the [OpenFiat](https://github.com/OpenFiat-org)
ecosystem — an open, decentralized peer-to-peer protocol for exchanging
stablecoins for local fiat currency. Solana secures asset settlement through
audited smart contracts; OpenFiat coordinates the peer-to-peer marketplace
layer (discovery, advertisements, reputation, governance, notifications, and
more) without centralized infrastructure.

This repository (Core) is the reference implementation: a real, working
node (`openfiat-node`) implementing every domain the protocol specifies —
advertisements, reservations, settlement, disputes, governance,
notifications, oracles, risk intelligence, snapshots — over a real
libp2p gossip mesh, plus a real JSON-RPC/WebSocket surface (OFS-8200) and
real Solana on-chain integration (escrow, staking, and governance
programs, deployed and exercised end to end against devnet). This is not
a scaffold — see [Guides](#guides) below for exactly how to run one.

For the full protocol motivation and design, see the
[whitepaper](https://github.com/OpenFiat-org/openfiat-specs) and the
[protocol specifications](https://github.com/OpenFiat-org/openfiat-specs/tree/main/Whitepaper/Specifications).

## Repository layout

```
.
├── Cargo.toml          # workspace manifest (31 crates)
├── deny.toml           # cargo-deny license/advisory policy
├── programs/            # separate Anchor workspace — on-chain escrow/staking/governance (OFS-4200)
├── crates/
│   ├── types/              serialization/         crypto/
│   ├── config/             storage/                database/
│   ├── network/            discovery/              gossip/
│   ├── chain/  (Solana chain bridge, OFS-4300)
│   ├── snapshot/           sessions/               registry/
│   ├── swqos/              identity/               wallet/
│   ├── reputation/         trade/                  advertisements/
│   ├── reservations/       settlement/             disputes/
│   ├── governance/         notifications/          oracles/
│   ├── risk/               rpc/                    api/
│   ├── metrics/            conformance/            cli/  (openfiat-node binary)
├── docs/
├── examples/
└── tests/
```

Each crate under `crates/` maps to one protocol component described in the
[OpenFiat whitepaper and specifications](https://github.com/OpenFiat-org/openfiat-specs).
See each crate's `src/lib.rs` doc comment for the specific `OFS-####` it
implements. `programs/` is a deliberately separate Cargo/Anchor workspace
(it pins its own Solana SDK versions) — see
[`programs/README.md`](programs/README.md).


## Quick start

```bash
git clone git@github.com:OpenFiat-org/openfiat-core.git
cd openfiat-core
cargo check --workspace
cargo run --bin openfiat-node
```

## Guides

- [**Getting started**](docs/getting-started.md) — the full walkthrough:
  building, running standalone, connecting to real Solana devnet,
  running a local multi-node cluster, and deploying/using the on-chain
  programs.
- [**Architecture**](docs/architecture.md) — crate dependency graph,
  wire format, transport, and canonical protocol parameters.
- [`programs/README.md`](programs/README.md) — the on-chain Anchor
  workspace: building, testing (`anchor test` against a real
  `solana-test-validator`), and deploying to devnet.
- [`CONFORMANCE.md`](CONFORMANCE.md) — what's proven to work end to end
  today, and against what.

## Running a node

`openfiat-node` is a real, standalone binary, configured entirely by
environment variables (no config file, no CLI flags) — see
[**Getting started §3**](docs/getting-started.md#3-run-standalone-gossip-only)
for the full list and their defaults:

```bash
cargo build --release --bin openfiat-node
./target/release/openfiat-node
```

For real Solana devnet connectivity, a local multi-node cluster, or
production deployment (systemd/Windows Service), see the
[**Getting started guide**](docs/getting-started.md) in full — it covers
every step, not just the summary here.


## Development

Requires a recent stable Rust toolchain (edition 2024, Rust 1.85+).

```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo check --workspace
```

Every crate implements real, tested business logic — not just
architecture scaffolding. `cargo doc --workspace --no-deps --open` is the
fastest way to browse what each one actually does; each crate's own
`src/lib.rs` doc comment names the specific `OFS-####` section it
implements. See [ROADMAP.md](ROADMAP.md) for what's left.


## Testing

```bash
cargo test --workspace --all-features
```

`crates/conformance` drives multi-node scenarios end to end (a real
gossip cluster, real advertisement → reservation → settlement flows, real
chain-bridge relay/confirmation, partition/recovery) — see
[`CONFORMANCE.md`](CONFORMANCE.md) for what's covered. The on-chain
programs (`programs/`) have their own `anchor test` suite against a real
`solana-test-validator` — see [`programs/README.md`](programs/README.md).


## Contributing

Contributions are welcome! Please read [CONTRIBUTING.md](CONTRIBUTING.md) and
our [Code of Conduct](CODE_OF_CONDUCT.md) before opening a pull request.
Security issues should be reported per [SECURITY.md](SECURITY.md), not as
public issues.

See [ROADMAP.md](ROADMAP.md) for current priorities and
[CHANGELOG.md](CHANGELOG.md) for release history.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
