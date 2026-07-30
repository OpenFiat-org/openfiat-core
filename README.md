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
cargo run --bin openfiat-node -- --help
```

See [**Running a node**](#running-a-node) below for a real invocation.

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

`openfiat-node` is a single standalone binary. **Every setting is a
command-line flag** — there is no config file and no environment-variable
fallback, deliberately: with two sources, a node's real configuration
becomes a function of the invocation *and* the ambient environment, and
"why does this node behave differently from the identical one beside it"
turns into archaeology across shell profiles and unit files.

`openfiat-node --help` is the complete surface.

### 1. Build

```bash
cargo build --release --bin openfiat-node
```

### 2. Create an identity

The node's wallet is its protocol identity **and the owner of its stake**.

```bash
solana-keygen new -o ~/openfiat/wallet.json
```

If the file is missing the node generates a throwaway key and says so
loudly, but does not save it — so its identity, and any stake bound to it,
would change on the next start. Back this file up somewhere off the
machine: **lose it and staked OPEN cannot be unbonded by anyone.**

### 3. Run

The smallest useful invocation — a gossip-only node, which learns on-chain
facts second-hand from peers:

```bash
./target/release/openfiat-node \
    --ledger ~/openfiat \
    --identity ~/openfiat/wallet.json
```

A **full node**, reading Solana directly and relaying other peers'
transactions:

```bash
./target/release/openfiat-node \
    --ledger ~/openfiat \
    --identity ~/openfiat/wallet.json \
    --solana-rpc-url https://api.devnet.solana.com \
    --entrypoint /ip4/<peer-host>/udp/4001/quic-v1
```

Check it:

```bash
curl -s -X POST http://127.0.0.1:7080/rpc \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"getHealth","params":{}}'
```

### Finding your own addresses

The node prints both of its identities at startup. They are the same key in
two encodings, and you need a different one for each job:

```
INFO openfiat_node: starting … address=RK5Yejkhtcm9tdBxYigwoyVFzLKSqRWunYXfNjFndbx
                                     peer_id=12D3KooWAEgFHnKTtntFZn8cb9U2ic4KkKNxUXVzsNuZ3Mt9gaKt
INFO openfiat_node: peers can reach this node at this address …
                    entrypoint=/ip4/0.0.0.0/udp/4098/quic-v1/p2p/12D3KooWAEgFHnKTtntFZn8cb9U2ic4KkKNxUXVzsNuZ3Mt9gaKt
```

- `address` is the Solana address that holds the node's stake — the string
  to give a faucet, paste into an explorer, or stake against. It is
  identical to `solana-keygen pubkey <your wallet.json>`.
- `entrypoint` is what another operator puts after their `--entrypoint` to
  dial you, **with the bind address replaced by a host they can route to**.
  A node binds `0.0.0.0` and cannot tell what a NAT or firewall makes of
  it, so substituting your real address is the one part it cannot do for
  you.

### Flags

| Flag | Default | What it does |
|---|---|---|
| `--ledger <DIR>` | `./openfiat-data` | RocksDB state and, by default, the identity keypair |
| `--identity <PATH>` | `<ledger>/wallet.json` | Solana CLI-format wallet.json; owns the node's stake |
| `--rpc-bind-address <HOST:PORT>` | `0.0.0.0:7080` | JSON-RPC and HTTP API (OFS-8200) |
| `--gossip-bind-address <MULTIADDR>` | `/ip4/0.0.0.0/udp/4001/quic-v1` | libp2p transport |
| `--entrypoint <MULTIADDR>` | none | Peer to dial at startup; repeatable |
| `--solana-rpc-url <URL>` | none | Repeatable. Any value puts the node in `RpcConnected` mode |
| `--solana-ws-url <URL>` | none | Recorded on the chain mode; nothing subscribes yet |
| `--snapshot-dir <DIR>` | `<ledger>/snapshots` | Where produced snapshots are written and served from |
| `--snapshot-public-url <URL>` | none | Repeatable. **Omitting it disables snapshot production** |
| `--snapshot-interval-secs <SECS>` | 3600 | Ignored without `--snapshot-public-url` |

Two behaviours worth knowing before you deploy:

- **No `--solana-rpc-url` means `GossipOnly`.** That is the safe default,
  not a broken state: the node still serves the marketplace and relays
  transactions to an RPC-connected peer. Its chain answers are second-hand
  and can lag.
- **Peer discovery does not run yet** ([#146]). A node finds only the peers
  given with `--entrypoint`, and announces no addresses of its own, so
  nothing will discover it either.

### Running under systemd

A production unit — dedicated user, hardening, restart-on-failure — is in
[`openfiat-infra/systemd/openfiat-node.service`](https://github.com/OpenFiat-org/openfiat-infra/blob/main/systemd/openfiat-node.service).

```bash
sudo useradd --system --home /var/lib/openfiat --shell /usr/sbin/nologin openfiat
sudo install -m 0755 target/release/openfiat-node /usr/local/bin/openfiat-node
sudo mkdir -p /var/lib/openfiat
sudo install -o openfiat -g openfiat -m 0600 wallet.json /var/lib/openfiat/wallet.json
sudo cp openfiat-node.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now openfiat-node
```

**`RestrictAddressFamilies` must include `AF_NETLINK`.** Binding a wildcard
address makes libp2p enumerate the host's interfaces, which on Linux goes
over a netlink socket; denied, the QUIC listener fails and the gossip actor
panics — while the HTTP thread survives, so `systemctl status` still reports
`active` and the node looks healthy while serving nothing. Always confirm
with a real request, not with the unit's status:

```bash
curl -s -X POST http://127.0.0.1:7080/rpc -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"getHealth","params":{}}'
```

For a local multi-node cluster and the on-chain programs, see the
[**Getting started guide**](docs/getting-started.md).

[#146]: https://github.com/OpenFiat-org/openfiat-core/issues/146


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
