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
    --entrypoint /dns4/openfiat.allenhark.com/udp/4001/quic-v1/p2p/12D3KooWK9hQ7TwbfvFiaAxUbRFCkdhS7iEpAJDnewNL1anyREQ1
```

That entrypoint is the public devnet node. A hostname is resolved at
startup through the system resolver, so the cluster survives its IP
changing; keep the `/p2p/<peer id>`, which is what makes a hijacked DNS
record fail the handshake rather than becoming your only peer. The IP
form `/ip4/84.32.223.111/...` works too.

To make your own node reachable from a browser it needs TLS — see
[`docs/getting-started.md`](docs/getting-started.md) §7, which covers
nginx, certbot, and `--public-rpc-url`.

Check it:

The public devnet node, if you just want to query one rather than run
one:

```bash
curl -s https://openfiat.allenhark.com/health   # ok
```

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
- `entrypoint` lines appear once the node is listening, one per address
  it is actually reachable at. They are what another operator puts after
  their own `--entrypoint`:

  ```
  INFO openfiat_rpc::actor: reachable at a new address …
       entrypoint=/ip4/203.0.113.9/udp/4001/quic-v1/p2p/12D3KooWAEgF…
  ```

  These are learned, not configured. `--gossip-bind-address` defaults to
  `0.0.0.0`, which means "every interface" to a listening socket and
  nothing to a dialing peer — so the node reports what libp2p actually
  bound (one line per interface) and, once a peer connects, the address
  that peer observed the connection arriving from. The second is the only
  way to learn a public address behind NAT: no amount of local inspection
  can produce it, because the translation happens elsewhere.

  Pick the one routable from wherever the other operator is. On a VPS with
  a public IP that is usually the first line; behind NAT, wait for a peer
  to report one, or forward a port and use that address.

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
| `--no-content-serving` | off | Stop holding and serving IPFS content. Costs the retrievability share of rewards |
| `--content-gateway <URL>` | Filebase | Where to fetch a block no peer has yet. Untrusted transport — bytes are checked against the CID |
| `--ipfs-api-url <URL>` | none | An existing Kubo cluster to pin content into *as well*. No longer how a node serves content |
| `--public-rpc-url <URL>` | none | This node's public HTTPS URL. Set it to advertise the node as reachable so browsers can use it |
| `--retention <DAYS\|archival>` | `30` | How long pinned content is kept. 30 days is the floor every node owes the network; shorter is refused |
| `--log <FILTER>` | `info` | Per-module directives accepted, e.g. `info,openfiat_rpc::actor=debug` |

Two behaviours worth knowing before you deploy:

- **No `--solana-rpc-url` means `GossipOnly`.** That is the safe default,
  not a broken state: the node still serves the marketplace and relays
  transactions to an RPC-connected peer. Its chain answers are second-hand
  and can lag.
- **Peer discovery does not run yet** ([#146]). A node finds only the peers
  given with `--entrypoint`, and announces no addresses of its own, so
  nothing will discover it either.
- **A node is a bounded storage commitment by default.** `--retention`
  keeps a rolling 30-day window and evicts past it; `--retention archival`
  keeps everything and is a deliberate choice. Not every node should carry
  the whole history. Challenges are only ever drawn from inside the 30-day
  floor, so evicting correctly never costs a node its reward share.
- **Content serving is on by default, and turning it off costs reward.**
  The node holds the content protocol records reference and answers
  bitswap for it over its own libp2p identity — no separate daemon, no
  extra process, no extra port. Answering a peer's retrievability
  challenge is what earns the full reward share; `--no-content-serving`
  stores nothing and earns a reduced share (0.7x, `[PROPOSED — NEEDS
  SIGN-OFF]`), because it is doing less for the network. Either way it
  still challenges its peers: measuring who serves content costs nothing
  and is a service in itself.

  This inverts an earlier opt-in, deliberately. A durability guarantee
  that required an operator to install a Go daemon is one almost nobody
  would have switched on, and a guarantee nobody opts into is not a
  guarantee. The operator who genuinely cannot spare the disk is the one
  who knows it.

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
