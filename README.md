<div align="center">

# openfiat-core

**Reference implementation of the OpenFiat protocol node, written in Rust.**

[![CI](https://github.com/OpenFiat-org/openfiat-core/actions/workflows/ci.yml/badge.svg)](https://github.com/OpenFiat-org/openfiat-core/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](LICENSE)
[![Discussions](https://img.shields.io/github/discussions/OpenFiat-org/openfiat-core)](https://github.com/orgs/OpenFiat-org/discussions)

[Website](https://openfiat.org) · [Docs](https://docs.openfiat.org) · [Specs](https://github.com/OpenFiat-org/openfiat-specs) · [Contributing](CONTRIBUTING.md)

</div>

---

## About

`openfiat-core` is part of the [OpenFiat](https://github.com/OpenFiat-org)
ecosystem — an open, decentralized peer-to-peer protocol for exchanging
stablecoins for local fiat currency. Solana secures asset settlement through
audited smart contracts; OpenFiat coordinates the peer-to-peer marketplace
layer (discovery, advertisements, reputation, governance, notifications, and
more) without centralized infrastructure.

This repository (Core) — reference implementation of the openfiat protocol node, written in rust.

For the full protocol motivation and design, see the
[whitepaper](https://github.com/OpenFiat-org/openfiat-specs) and the
[protocol specifications](https://github.com/OpenFiat-org/openfiat-specs/tree/main/Whitepaper/Specifications).

## Repository layout

```
.
├── Cargo.toml          # workspace manifest (29 crates)
├── deny.toml           # cargo-deny license/advisory policy
├── crates/
│   ├── types/              serialization/         crypto/
│   ├── config/             storage/                database/
│   ├── network/            discovery/              gossip/
│   ├── snapshot/           sessions/               registry/
│   ├── swqos/              identity/               wallet/
│   ├── reputation/         trade/                  advertisements/
│   ├── reservations/       settlement/             disputes/
│   ├── governance/         notifications/          oracles/
│   ├── risk/               rpc/                    api/
│   ├── metrics/            cli/  (openfiat-node binary)
├── docs/
├── examples/
└── tests/
```

Each crate under `crates/` maps to one protocol component described in the
[OpenFiat whitepaper and specifications](https://github.com/OpenFiat-org/openfiat-specs).
See each crate's `src/lib.rs` doc comment for the specific `OFS-####` it
implements.


## Quick start

```bash
git clone git@github.com:OpenFiat-org/openfiat-core.git
cd openfiat-core
cargo check --workspace
cargo run --bin openfiat-node
```


## Development

Requires a recent stable Rust toolchain (edition 2024, Rust 1.85+).

```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo check --workspace
```

This repository currently contains **architecture only** — crate
boundaries, module layout, and public surface — with no business logic.
See [ROADMAP.md](ROADMAP.md) for implementation sequencing.


## Testing

```bash
cargo test --workspace --all-features
```

Protocol-level conformance vectors (once implementations land) are exercised
via [openfiat-conformance](https://github.com/OpenFiat-org/openfiat-conformance)
rather than duplicated in this repository.


## Contributing

Contributions are welcome! Please read [CONTRIBUTING.md](CONTRIBUTING.md) and
our [Code of Conduct](CODE_OF_CONDUCT.md) before opening a pull request.
Security issues should be reported per [SECURITY.md](SECURITY.md), not as
public issues.

See [ROADMAP.md](ROADMAP.md) for current priorities and
[CHANGELOG.md](CHANGELOG.md) for release history.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
