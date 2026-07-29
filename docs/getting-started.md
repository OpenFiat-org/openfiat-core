# Getting started — openfiat-core

The real, end-to-end process for building, running, and deploying an
`openfiat-node`. Every command and default below is taken directly from
the current implementation (`crates/cli/src/main.rs`, `packaging/`,
`openfiat-infra/docker/`) — not aspirational.

## 1. Prerequisites

- Rust, edition 2024 (Rust 1.85+) — `rustup` picks up the pinned toolchain
  automatically from `rust-toolchain.toml`.
- Optional, only if this node will talk to Solana (see §4 below):
  [Solana CLI](https://docs.anza.xyz/cli/install), devnet-configured.

## 2. Build

```bash
git clone git@github.com:OpenFiat-org/openfiat-core.git
cd openfiat-core
cargo build --release --bin openfiat-node
```

The binary is a real, standalone executable at
`target/release/openfiat-node` — no container runtime or config file
required to run it.

## 3. Run standalone (gossip-only)

`openfiat-node` takes **no command-line flags** — every setting is an
environment variable, read once at startup
(`crates/cli/src/main.rs`). With nothing set, it generates a fresh
identity, listens for peers, and serves its HTTP surface with no Solana
RPC connectivity (`NodeChainMode::GossipOnly`, OFS-4300 §4):

```bash
./target/release/openfiat-node
```

```
openfiat-node 0.1.0 — data dir: ./openfiat-data, gossip identity: <peer-id>, chain mode: GossipOnly
openfiat-node listening on http://0.0.0.0:7080 (try GET /health, GET /docs)
```

Verify it's actually up:

```bash
curl http://localhost:7080/health
curl -X POST http://localhost:7080/rpc -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"getVersion","params":{}}'
```

Every environment variable this binary reads, with its real default:

| Variable | Default if unset | What it controls |
|---|---|---|
| `CLI_WALLET_PATH` | `~/.config/solana/id.json` | This node's identity — a Solana CLI-format `wallet.json` (`solana-keygen new` output). Reused as both the node's gossip/P2P keypair and its Solana signing key. Missing/unreadable → a fresh throwaway identity is generated for that run (fine for local testing, **not** for anything you want to persist). |
| `CLI_DATA_DIR` | `./openfiat-data` | RocksDB data directory — every domain registry's persisted state. |
| `CLI_HTTP_ADDR` | `0.0.0.0:7080` | Bind address for the JSON-RPC (`POST /rpc`), WebSocket (`GET /ws`), and REST (`crates/api`) surface. |
| `CLI_LISTEN_ADDR` | `/ip4/0.0.0.0/udp/4001/quic-v1` | libp2p gossip listen multiaddr (QUIC). |
| `CLI_BOOTSTRAP_PEERS` | *(empty)* | Comma-separated multiaddrs to dial on startup, e.g. `/ip4/203.0.113.10/udp/4001/quic-v1`. Empty means this node is its own bootstrap (fine for a lone node or the first node in a new cluster). |
| `CLI_SOLANA_RPC_URLS` | *(empty)* | Comma-separated Solana RPC endpoint(s). **Unset is the default and stays `GossipOnly`** — set this to opt into `NodeChainMode::RpcConnected` (§4 below). |
| `CLI_SOLANA_WS_URL` | *(empty)* | Optional Solana WebSocket endpoint, recorded but not yet used for subscription-based polling. |
| `CLI_STAKING_PROGRAM_ID` | *(empty)* | Base58 program id of the deployed `openfiat-staking` program. Required for this node to independently verify a governance vote's real on-chain stake weight (OFS-4000, Phase 6's `poll_vote_verifications`) — without it, every pending vote verification is left queued rather than ever trusted. See §5 for the real devnet id. |

## 4. Run with real Solana devnet connectivity

Set `CLI_SOLANA_RPC_URLS` to put the node in `RpcConnected` mode — it will
poll for a fresh blockhash, relay signed transactions
(`sendTransaction`), and answer `getChainStatus`/`getLatestBlockhash` for
real:

```bash
CLI_SOLANA_RPC_URLS=https://api.devnet.solana.com \
CLI_STAKING_PROGRAM_ID=HYEXk8XQukBkZbiYB33JyVefQDxqyCpPudad3wBCyYmx \
./target/release/openfiat-node
```

```bash
curl -X POST http://localhost:7080/rpc -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"getChainStatus","params":{}}'
# {"jsonrpc":"2.0","id":1,"result":{"mode":"RpcConnected","blockhash":"...","slot":...,"age_ms":...}}
```

Never commit a real RPC endpoint/API key to version control — set it via
environment only (`packaging/systemd/node.env.example` shows the
production convention: an `/etc/openfiat/node.env` file readable only by
the service's own user).

## 5. The on-chain programs this node talks to

Three Anchor programs are deployed to devnet today (see
`programs/README.md` and `programs/devnet-addresses.json` for the full,
current list — the table below is a snapshot, not the source of truth):

| Program | Devnet program id | OFS spec |
|---|---|---|
| `openfiat-escrow` | `HaPpM1QYM3dKp3sX7zhEdft9hB6ncu6xfALAbkyQChQP` | OFS-4200 §4 |
| `openfiat-staking` | `HYEXk8XQukBkZbiYB33JyVefQDxqyCpPudad3wBCyYmx` | OFS-4200 §5 |
| `openfiat-governance` | `AVJfKUjHsizkGGUy8sdz4Xma2hVgmgvgg8GmUMs8E4eE` | OFS-4200 §6 |

A node never builds or signs an on-chain transaction on a caller's
behalf — a client (using `openfiat-sdk`'s `onchain` module, or any
Anchor-compatible tooling pointed at OFS-4200's account layouts) builds
and signs a transaction itself, then submits it via this node's
`sendTransaction` (optionally tagged with a `"<domain>:<id>"`
correlation — see `crates/rpc::methods::chain`'s own doc comment — so
this node routes the eventual confirmation into the right off-chain
registry, e.g. `SettlementRegistry`/`DisputeRegistry`).

To build/deploy the programs yourself (rather than using the existing
devnet deployment above), see `programs/README.md` in full — it covers
the Anchor toolchain, `anchor test`, and `anchor deploy
--provider.cluster devnet`.

## 6. Run a local multi-node cluster

A real, persistently-running 3-node cluster (one `RpcConnected` bootstrap
node plus two `GossipOnly` followers, each a genuine `openfiat-node`
process with its own identity and RocksDB volume) is available via
Docker Compose in
[`openfiat-infra/docker`](https://github.com/OpenFiat-org/openfiat-infra/tree/main/docker):

```bash
git clone git@github.com:OpenFiat-org/openfiat-infra.git
cd openfiat-infra/docker
docker compose -f docker-compose.dev.yml up
```

Node 0 is reachable at `http://localhost:7080`, node 1 at
`http://localhost:7081`, node 2 at `http://localhost:7082`. Override
`CLI_SOLANA_RPC_URLS` via a local, untracked `.env` file in that
directory to use a faster private RPC endpoint than Solana's public
devnet one.

## 7. Production deployment

- **Linux (systemd)** — [`packaging/systemd/README.md`](../packaging/systemd/README.md):
  a real unit file with auto-restart and graceful `SIGTERM` shutdown, an
  `/etc/openfiat/node.env` convention for secrets, and `ufw` firewall
  rules for the HTTP (7080/tcp) and gossip (4001/udp) ports.
- **Windows** — [`packaging/windows/README.md`](../packaging/windows/README.md):
  running the same binary as a real Windows Service via NSSM.

Both start from the exact same binary and environment-variable surface
documented in §3 above — nothing production-specific changes about how
the node itself is configured.

## 8. Next steps

- [`architecture.md`](architecture.md) — crate dependency graph, wire
  format, transport, and canonical protocol parameters.
- [`programs/README.md`](../programs/README.md) — the on-chain Anchor
  workspace in full: building, testing, and deploying
  escrow/staking/governance.
- [`CONFORMANCE.md`](../CONFORMANCE.md) — what's actually proven to work
  end to end today, and against what (a real local validator vs. real
  devnet).
- [openfiat-sdks](https://github.com/OpenFiat-org/openfiat-sdks) — typed
  Rust/TypeScript clients for both this node's JSON-RPC surface and the
  on-chain programs' instructions.
