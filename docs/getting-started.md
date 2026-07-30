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
| `--identity <PATH>` | `<ledger>/wallet.json` | This node's identity — a Solana CLI-format `wallet.json` (`solana-keygen new` output). Reused as both the node's gossip/P2P keypair and its Solana signing key. Missing/unreadable → a fresh throwaway identity is generated for that run (fine for local testing, **not** for anything you want to persist). |
| `--ledger <DIR>` | `./openfiat-data` | RocksDB data directory — every domain registry's persisted state. |
| `--rpc-bind-address <HOST:PORT>` | `0.0.0.0:7080` | Bind address for the JSON-RPC (`POST /rpc`), WebSocket (`GET /ws`), and REST (`crates/api`) surface. |
| `--gossip-bind-address <MULTIADDR>` | `/ip4/0.0.0.0/udp/4001/quic-v1` | libp2p gossip listen multiaddr (QUIC). |
| `--entrypoint <MULTIADDR>` | *(none)* | Peer to dial on startup; repeat the flag for several, e.g. `/ip4/203.0.113.10/udp/4001/quic-v1`. Empty means this node is its own bootstrap (fine for a lone node or the first node in a new cluster). |
| `--solana-rpc-url` | *(empty)* | Comma-separated Solana RPC endpoint(s). **Unset is the default and stays `GossipOnly`** — set this to opt into `NodeChainMode::RpcConnected` (§4 below). |
| `--solana-ws-url` | *(empty)* | Optional Solana WebSocket endpoint, recorded but not yet used for subscription-based polling. |

Every variable above is *operational* — it decides how this node reaches
the network, never what the network's rules are. Program ids, PDA seeds
and the OPEN mint are compiled in (§5) and are deliberately not settable
here: a node that could be pointed at a different staking program could
be made to count governance votes weighted by stake that does not exist.

## 4. Run with real Solana devnet connectivity

Set `--solana-rpc-url` to put the node in `RpcConnected` mode — it will
poll for a fresh blockhash, relay signed transactions
(`sendTransaction`), and answer `getChainStatus`/`getLatestBlockhash` for
real:

```bash
--solana-rpc-url=https://api.devnet.solana.com \
./target/release/openfiat-node
```

```bash
curl -X POST http://localhost:7080/rpc -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"getChainStatus","params":{}}'
# {"jsonrpc":"2.0","id":1,"result":{"mode":"RpcConnected","blockhash":"...","slot":...,"age_ms":...}}
```

An endpoint that is not JSON-RPC is refused at startup rather than
crashing the node later. `solana-client` reads a response as
`value["result"]`, which panics on anything else — and the panic kills the
chain thread while the HTTP server keeps serving, so the node reports
itself healthy while doing nothing on chain. A common way to hit this is
Helius's Enhanced Transactions path (`/v0/transactions/`), which returns
a bare JSON array; use the plain endpoint.

Never commit a real RPC endpoint or API key. It is a command-line flag,
so it belongs in the unit file that starts the node (`systemctl cat
openfiat-node` shows exactly what a running node was given) and in
nothing that is version controlled.

## 5. Pin content, and earn more for it

A node can hold the files protocol records reference — dispute evidence,
merchant avatars — and prove it. Opt in by pointing the node at an IPFS
daemon:

```bash
# A local Kubo daemon, the usual case.
ipfs daemon &

./target/release/openfiat-node \
    --ledger ~/openfiat \
    --identity ~/openfiat/wallet.json \
    --solana-rpc-url https://api.devnet.solana.com \
    --ipfs-api-url http://127.0.0.1:5001
```

What this changes:

- The node pins the content referenced by attachment records it has
  accepted, so evidence stays retrievable after the uploader stops paying
  for it — which matters, because a dispute can open weeks after a trade.
- It keeps a local copy of the part small enough to verify (256 KiB, the
  IPFS chunk boundary) so it can answer another node's retrievability
  challenge from memory.
- **It earns the full reward share.** Peers challenge each other by asking
  for content by CID and hashing what comes back; a content address is the
  hash of its content, so the right bytes cannot be produced without
  having them. A node that answers keeps its full multiplier, and one that
  cannot is scaled to 0.7 (`[PROPOSED — NEEDS SIGN-OFF]`). See
  [OFS-4100 §9.2] and `crates/rewards`.

Without `--ipfs-api-url` a node stores no content and earns the reduced
share, which is the honest outcome — it is doing less for the network. It
still challenges its peers either way: measuring who serves content costs
nothing and is a service in itself.

Check what your node is holding:

```bash
curl -s -X POST http://localhost:7080/rpc -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"getHeldContent","params":{"cid":"bafkrei..."}}'
# {"jsonrpc":"2.0","id":1,"result":{"content":"<base64>"}}   ← held
# {"jsonrpc":"2.0","id":1,"result":{"content":null}}          ← not held
```

Pinning is opt-in rather than automatic on purpose. A node that fetched
every CID it saw would store whatever anyone chose to point it at. What
bounds the opted-in case is that an attachment must name a settlement, and
a settlement needs a real reservation against real escrow — so the ceiling
on what your disk is asked for is the network's actual trading volume, not
a stranger's patience.

### How long content is kept

Not every node should carry the whole history. `--retention` defaults to
**30 days**, so running a node is a bounded storage commitment rather than
an open-ended one; content older than the window is evicted on the next
pinning sweep.

```bash
--retention 30          # the default: a rolling 30-day window
--retention 365         # a longer window, still rolling
--retention archival    # keep everything, forever — an explicit choice
```

30 days is also the **floor every node owes the network**, so shorter
values are refused rather than quietly raised — a node configured for
seven days that silently ran for thirty would be doing something other
than what its operator asked.

That floor is what lets eviction and rewards coexist. Challenges are only
ever drawn from content inside it, so a rolling node that correctly
evicted last year's evidence is never asked about it and never loses its
share for having done the right thing. Equally, no node can shrink what it
can be asked by declaring a smaller window.

[OFS-4100 §9.2]: https://github.com/OpenFiat-org/openfiat-specs

## 6. Joining the devnet cluster

Peer discovery does not run yet ([#146]), so a node finds only the peers
it is given. The public devnet entrypoint is:

```
/ip4/84.32.223.111/udp/4001/quic-v1/p2p/12D3KooWK9hQ7TwbfvFiaAxUbRFCkdhS7iEpAJDnewNL1anyREQ1
```

```bash
./target/release/openfiat-node \
    --ledger ~/openfiat \
    --identity ~/openfiat/wallet.json \
    --solana-rpc-url https://api.devnet.solana.com \
    --entrypoint /ip4/84.32.223.111/udp/4001/quic-v1/p2p/12D3KooWK9hQ7TwbfvFiaAxUbRFCkdhS7iEpAJDnewNL1anyREQ1
```

Repeat `--entrypoint` for several. Your own node logs the addresses it is
reachable at once it is listening — see §3 — and those are what you give
another operator.

[#146]: https://github.com/OpenFiat-org/openfiat-core/issues/146

## 7. The on-chain programs this node talks to

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

## 8. Run a local multi-node cluster

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
`--solana-rpc-url` via a local, untracked `.env` file in that
directory to use a faster private RPC endpoint than Solana's public
devnet one.

## 9. Turning things off

Most of what a node does is not optional — it gossips, validates and
serves whatever it has replicated. What *is* optional is listed here, so
you can run the smallest useful node deliberately rather than by
discovering which flags to leave out.

| To disable | Do this | What you lose |
|---|---|---|
| Solana connectivity | omit `--solana-rpc-url` | Node runs `GossipOnly`: on-chain answers come second-hand from peers and can lag. It still serves the marketplace and relays transactions to an RPC-connected peer. Earns the reduced connectivity share. |
| Content pinning | omit `--ipfs-api-url` | Node stores no attachment content and cannot answer a retrievability challenge, so it earns the reduced share (0.7x). It still challenges its peers. |
| Keeping old content | `--retention 30` (the default) | Nothing, unless you were relying on this node to serve older-than-30-day content. Use `--retention archival` if you intend to run an archive. |
| Producing snapshots | omit `--snapshot-public-url` | Node produces no snapshots for others to bootstrap from. It still *consumes* them, which needs no configuration. |
| Log volume | `--log warn` | Routine INFO lines, including the addresses the node is reachable at. Errors and warnings still appear. |

Two things you cannot turn off, and should not want to: signature
verification on every event, and the compile-time program IDs. See
`crates/chain/src/programs.rs` for why the second is not configuration.

Nothing here is an environment variable. `openfiat-node --help` is the
whole surface.

## 10. Production deployment

- **Linux (systemd)** — [`packaging/systemd/README.md`](../packaging/systemd/README.md):
  a real unit file with auto-restart and graceful `SIGTERM` shutdown,
  every setting as a flag on `ExecStart`, and `ufw` firewall rules for the
  HTTP (7080/tcp) and gossip (4001/udp) ports.
- **Windows** — [`packaging/windows/README.md`](../packaging/windows/README.md):
  running the same binary as a real Windows Service via NSSM.

Both start from the exact same binary and the same command-line flags
documented above — nothing production-specific changes about how the node
is configured. There is no environment-variable fallback and no config
file: `openfiat-node --help` is the entire surface, deliberately, so a
node's behaviour is a function of its invocation and nothing ambient.

## 11. Next steps

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
