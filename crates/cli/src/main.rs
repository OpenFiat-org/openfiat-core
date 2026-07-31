//! `openfiat-node` — composition root wiring the crates above into one
//! running node: a real RocksDB-backed store, every domain registry
//! `openfiat-rpc`'s `NodeState` composes over a real, gossip-connected
//! `GossipService`, and the merged `rpc`+`api` axum server bound to a
//! real HTTP port.
//!
//! Node identity is a Solana CLI-format wallet.json (the same file
//! `solana-keygen new` produces), the same convention
//! `openfiat-apps/explorer/indexer` already uses — see
//! `load_or_generate_wallet` below. That same identity (via its seed)
//! is reused as this node's gossip/network keypair, so an operator's
//! one wallet authenticates both the Solana tooling and this node's
//! P2P identity.
//!
//! This node claims every "gateway" role gossip's origination
//! authorization (OGP §7, `openfiat_gossip::authorization`) checks —
//! `MerchantGateway`, `OracleProvider`, `NotificationGateway`,
//! `RiskIntelligenceProvider` — because it accepts and relays signed
//! `sendX` submissions on behalf of whichever external wallet actually
//! signed each one (see `crates/rpc/src/dispatch.rs::originate`'s own
//! doc comment). The role check governs *this node's* standing to put
//! an event on the wire, not the submitter's identity — that is, and
//! remains, verified by each domain's own `apply_*` before origination
//! is ever attempted.
//!
//! Every flag this module accepts is **operational** — where this node
//! listens, where it keeps its data, which peers and RPC endpoints it
//! talks to. None of them can change what this node believes.
//!
//! Configuration is command-line arguments and nothing else. There is no
//! environment-variable fallback and no config file, deliberately: with
//! two sources a node's actual settings become a function of the
//! invocation AND the ambient environment, and "why is this node behaving
//! differently from the identical one beside it" turns into an
//! archaeology exercise across shell profiles, unit files and container
//! environments. `openfiat-node --help` is the whole surface, and
//! `systemctl cat openfiat-node` shows exactly what a running node was
//! given. Protocol identity (program ids, the OPEN mint, PDA seeds,
//! account discriminators) is pinned at compile time in
//! `openfiat_chain::programs` and is deliberately unreachable from the
//! environment; protocol parameters (fees, stake minimums, quorum) live
//! on chain under governance control. The rule those three cases follow:
//! if two honest nodes running the same release could disagree because of
//! a value, that value is not configuration.

use clap::Parser;
use openfiat_chain::NodeChainMode;
use openfiat_content::Retention;
use openfiat_crypto::Keypair;
use openfiat_database::Database;
use openfiat_network::Multiaddr;
use openfiat_network::identity::to_libp2p_keypair;
use openfiat_rpc::NetworkConfig;
use openfiat_types::NodeRole;
use openfiat_wallet::Wallet;
use std::sync::Arc;

/// Every column family a domain registry composed into `NodeState`
/// opens — see each crate's `store.rs` for its own `COLUMN_FAMILY`
/// constant. `openfiat_database::Database::open` requires the full set
/// up front, unlike `MemoryStore`, which opens column families lazily,
/// and a `put` to one that was never opened fails silently at every
/// call site in this workspace (they all discard the result).
///
/// This is the snapshotted set (`openfiat_rpc::SNAPSHOT_COLUMN_FAMILIES`,
/// the single definition of what makes up this node's worldview) plus the
/// three that are deliberately node-local and so are not snapshotted:
/// the gossip event log and the two snapshot bookkeeping families.
///
/// `pinned_content` used to be here too. It is snapshotted now — a node
/// bootstrapping from a peer comes up able to serve the evidence its
/// records reference instead of refetching it for hours — so it comes
/// from the snapshotted set, and listing it twice would open the same
/// column family twice.
///
/// `peers` was missing from both lists, which is the failure this
/// constant's own doc comment describes, made real: `PeerCache::upsert`
/// discards its result, so on a RocksDB node every write of a discovered
/// peer failed silently and `getPeers` answered with an empty list
/// forever — on a node that was connected to a peer at that moment. The
/// network looked like one node because nothing could record the second.
/// Node-local rather than snapshotted: who this node happens to be
/// connected to is not part of the worldview it replicates, and handing a
/// bootstrapping node somebody else's peer table would tell it about
/// connections it does not have.
const NODE_LOCAL_COLUMN_FAMILIES: &[&str] = &[
    "gossip_events",
    "snapshot_metadata",
    "snapshot_checkpoint",
    "peers",
];

fn column_families() -> Vec<&'static str> {
    openfiat_rpc::SNAPSHOT_COLUMN_FAMILIES
        .iter()
        .chain(NODE_LOCAL_COLUMN_FAMILIES)
        .copied()
        .collect()
}

/// `openfiat-node` — a node in the OpenFiat network.
///
/// Every setting is a flag. See the module doc for why there is no
/// environment-variable fallback and no config file.
#[derive(Debug, Parser)]
#[command(name = "openfiat-node", version, about = "Run an OpenFiat node")]
pub struct Args {
    /// Directory for this node's RocksDB state and its identity keypair.
    #[arg(long, value_name = "DIR", default_value = "./openfiat-data")]
    pub ledger: String,

    /// Solana CLI-format wallet.json used as this node's identity.
    ///
    /// This key owns the node's stake. Defaults to `<ledger>/wallet.json`;
    /// a fresh one is generated there if absent, and losing it strands any
    /// staked OPEN with no way to unbond.
    #[arg(long, value_name = "PATH")]
    pub identity: Option<String>,

    /// Address for the JSON-RPC and HTTP API (OFS-8200).
    ///
    /// 7080 rather than 8080: that port is crowded on a typical server, and
    /// a node silently losing the bind race is a bad first five minutes.
    #[arg(long, value_name = "HOST:PORT", default_value = "0.0.0.0:7080")]
    pub rpc_bind_address: String,

    /// Multiaddr the libp2p transport binds.
    #[arg(
        long,
        value_name = "MULTIADDR",
        default_value = "/ip4/0.0.0.0/udp/4001/quic-v1"
    )]
    pub gossip_bind_address: Multiaddr,

    /// Peer to dial on startup. Repeat for several.
    ///
    /// A hostname works: `/dns4/openfiat.allenhark.com/udp/4001/quic-v1/p2p/<peer id>`
    /// is resolved at startup through the operating system's own resolver.
    /// Keep the `/p2p/<peer id>` — DNS is not authenticated, and the peer
    /// id is what makes a hijacked record fail the handshake instead of
    /// silently becoming your only peer.
    ///
    /// A lone node has nothing to dial. Peer discovery spreads knowledge
    /// between nodes that are already connected, so it finds the rest of
    /// the network from here — but something has to make the first
    /// connection, and this is it.
    #[arg(long = "entrypoint", value_name = "MULTIADDR")]
    pub entrypoints: Vec<Multiaddr>,

    /// Solana RPC endpoint. Repeat for several; relayed transactions go to
    /// all of them in parallel (OFS-4300 §7).
    ///
    /// Supplying at least one puts this node in `RpcConnected` mode. With
    /// none it is `GossipOnly` — the safe, zero-config default, where
    /// on-chain facts are learned second-hand over gossip. An operator opts
    /// into a real chain connection explicitly.
    #[arg(long = "solana-rpc-url", value_name = "URL")]
    pub solana_rpc_urls: Vec<String>,

    /// Solana websocket endpoint. Recorded on the chain mode; no
    /// subscription-based polling reads it yet.
    #[arg(long, value_name = "URL")]
    pub solana_ws_url: Option<String>,

    /// Where snapshots this node produces are written, and the only
    /// directory `GET /snapshot/{id}` serves. Defaults to
    /// `<ledger>/snapshots`.
    #[arg(long, value_name = "DIR")]
    pub snapshot_dir: Option<String>,

    /// Override where other nodes fetch this node's snapshots from.
    /// Repeat for several.
    ///
    /// Not normally needed. The node derives this from the addresses it
    /// learns it is reachable at — its bound interfaces, and the address a
    /// peer reports seeing it from, which is what sees through NAT — plus
    /// the `--rpc-bind-address` port that already serves `GET /snapshot`.
    ///
    /// Set it when the node genuinely cannot: an RPC port reached through
    /// a reverse proxy on a different port or hostname, where what the node
    /// observes about itself is not what a peer must ask for.
    #[arg(long = "snapshot-public-url", value_name = "URL")]
    pub snapshot_public_urls: Vec<String>,

    /// Seconds between snapshots. Defaults to an hour.
    #[arg(long, value_name = "SECS")]
    pub snapshot_interval_secs: Option<u64>,

    /// Additionally trust this base58 public key to supply this node's
    /// FIRST snapshot. Repeat for several.
    ///
    /// Only ever needed by a node with no history of its own. Every check
    /// on an import — signature, registration, size, state root —
    /// establishes that the bytes are what the announcer said, not that
    /// the announcer is honest, and a node with no checkpoint has nothing
    /// to judge that against. So a first snapshot must come from a pinned
    /// anchor; after it, any registered provider will do.
    ///
    /// This ADDS to the pinned anchors and cannot remove them. Trust your
    /// own infrastructure with it; nothing you can set here un-trusts the
    /// keys compiled in, which is the first thing a tampered
    /// configuration would try.
    #[arg(long = "trusted-snapshot-provider", value_name = "PUBKEY")]
    pub trusted_snapshot_providers: Vec<String>,

    /// Stop producing snapshots.
    ///
    /// Production is ON by default: a network where nobody snapshots is a
    /// network no new node can join without replaying all history, and a
    /// feature every operator has to opt into is a feature most of them
    /// will not. The cost is one full read of this node's state per
    /// interval and a few files on disk (`--snapshot-dir`, three retained).
    ///
    /// Pass this if that disk or that read genuinely is not there. This
    /// node then still *consumes* snapshots — bootstrapping from a peer
    /// has never needed configuration.
    #[arg(long)]
    pub no_snapshot_production: bool,

    /// Stop holding and serving protocol content.
    ///
    /// Content serving is ON by default. The node holds the attachments
    /// its retention window covers and answers any IPFS peer that asks
    /// for one, over its own libp2p identity — no separate daemon, no
    /// extra process, no extra port.
    ///
    /// Turning it off costs reward. Holding and serving content is what
    /// earns the content-retrievability share (OFS-4100 §9.2); a node
    /// that stores nothing cannot answer a retrievability challenge and
    /// earns the reduced multiplier, which is the honest outcome, since
    /// it is doing less for the network. Pass this only if the disk
    /// genuinely is not there.
    ///
    /// It still challenges its peers either way: measuring who serves
    /// content is a service a node performs whether or not it stores any
    /// itself.
    #[arg(long)]
    pub no_content_serving: bool,

    /// Stop announcing held content on the public IPFS DHT.
    ///
    /// Announcing is ON whenever the node serves content, because serving
    /// without announcing is a durability guarantee only peers that
    /// already know this node can use. A gateway — which is what a
    /// browser actually fetches an attachment through — finds a provider
    /// through the DHT or not at all.
    ///
    /// What publishing discloses is this node's peer id and its dialable
    /// addresses, globally, to anyone who asks the DHT for the content.
    /// That is the entire point of it, and it is also a disclosure an
    /// operator may not want: it makes the machine addressable by
    /// strangers rather than only by peers it has been introduced to.
    ///
    /// Passing this costs no reward. A retrievability challenge arrives
    /// over the node's registered JSON-RPC endpoint, not over the DHT, so
    /// a node that holds content and declines to advertise it still
    /// answers and still earns the full share. What it gives up is being
    /// found by third parties.
    #[arg(long)]
    pub no_content_announce: bool,

    /// Where to fetch a block no peer has yet. Defaults to a public IPFS
    /// gateway.
    ///
    /// Bitswap moves blocks between peers that already have them; it does
    /// not create the first copy. Attachments enter the network through a
    /// pinning service, so the first node to want one fetches it from the
    /// wider IPFS network here, checks it against the CID, and serves it
    /// to its peers from then on. The gateway is untrusted transport —
    /// bytes that are not what the CID names are refused — but it does
    /// learn which content this node asks for, which is why an operator
    /// can point it at their own.
    #[arg(long, value_name = "URL", default_value = openfiat_content::DEFAULT_GATEWAY)]
    pub content_gateway: String,

    /// An IPFS daemon to *also* pin protocol content into, e.g.
    /// `http://127.0.0.1:5001`.
    ///
    /// No longer how a node serves content — it serves in process now —
    /// but an operator who already runs a Kubo cluster can have protocol
    /// content pinned into it as well, putting a copy somewhere this
    /// node's own retention window does not govern.
    #[arg(long, value_name = "URL")]
    pub ipfs_api_url: Option<String>,

    /// A multiaddr peers should dial to reach this node's gossip port,
    /// e.g. `/ip4/203.0.113.7/udp/4001/quic-v1`. Repeatable.
    ///
    /// Needed when the address the node binds is not the address peers can
    /// reach it at — behind NAT, inside a container, or on a cloud host
    /// with a mapped public IP. The node cannot work this out for itself:
    /// only something on the far side of the NAT can observe the public
    /// address, so it is declared rather than guessed.
    ///
    /// Announced ahead of the bound addresses, so a peer trying them in
    /// order reaches the node immediately instead of timing out on a
    /// private one. The bound addresses are still announced too — peers on
    /// the same LAN or docker network can only reach each other that way.
    ///
    /// Omit it if the node is genuinely on a public interface. Its bound
    /// address already is its public one.
    #[arg(long = "external-addr", value_name = "MULTIADDR")]
    pub external_addrs: Vec<Multiaddr>,

    /// Where this node's earnings are paid, as a Solana address.
    ///
    /// Defaults to this node's own identity address — the key in
    /// `--identity`, which the node demonstrably controls because it signs
    /// every event with it. A node has always had a wallet; registering
    /// without one meant a node doing real work had nowhere for its share
    /// to go.
    ///
    /// Set this to a wallet you keep elsewhere if you would rather not
    /// accrue earnings to a key that lives unencrypted on a server. The
    /// node never needs the payout wallet's private key — it only names
    /// the address.
    #[arg(long, value_name = "ADDRESS")]
    pub payout_wallet: Option<String>,

    /// A region to declare, e.g. `eu-west` or `KE`.
    ///
    /// Self-declared and unverified — a client may prefer a nearby node,
    /// and nothing here proves where this one is. Omit it rather than
    /// guessing; a node on a laptop has no useful region, and an absent
    /// answer is better than a wrong one.
    ///
    /// It stays self-declared on purpose. #173 asked whether the node
    /// could work its own region out from the addresses it announces;
    /// `docs/region-is-declared.md` records why deriving it would
    /// produce a confident answer to a different question.
    #[arg(long, value_name = "REGION")]
    pub region: Option<String>,

    /// A name for this node in directories, e.g. `AllenHark EU`.
    ///
    /// Self-asserted, like everything below it: the registration is
    /// signed by this node's key, which proves the record was not
    /// altered and proves nothing about whether the name is yours to
    /// use. Clients show it beside the peer id, never instead of it.
    ///
    /// Capped at 64 characters, because every node on the network stores
    /// what you write here until this node deregisters.
    #[arg(long, value_name = "NAME")]
    pub node_name: Option<String>,

    /// A sentence about what this node is for, up to 280 characters.
    #[arg(long, value_name = "TEXT")]
    pub node_description: Option<String>,

    /// A logo for this node, as an IPFS CID — not a URL.
    ///
    /// A URL would make everyone who so much as scrolls past this node
    /// in a directory issue a request to whoever hosts the image, which
    /// hands that host their IP address and what they were looking at. A
    /// CID names one specific image and is served by the node the viewer
    /// already chose to talk to, over its `GET /ipfs/{cid}`.
    ///
    /// Publish the image to this network first — `POST /api/ipfs/upload`
    /// in the web app, or any node's content API — and pass the CID it
    /// returns. A value that is not a CIDv1 is refused rather than
    /// registered.
    #[arg(long, value_name = "CID")]
    pub node_logo: Option<String>,

    /// A website for whoever runs this node, e.g.
    /// `https://allenhark.com`.
    ///
    /// Not `--public-rpc-url`. That one is where this node's API lives
    /// and other software dials it; this one is a link a human clicks.
    /// Naming them apart is deliberate — an operator who swapped them
    /// would publish their marketing site as a dialable endpoint.
    #[arg(long, value_name = "URL")]
    pub node_website: Option<String>,

    /// This node's publicly reachable API URL, e.g.
    /// `https://openfiat.allenhark.com`.
    ///
    /// Set it once the node is behind TLS and reachable from the internet,
    /// and the node advertises itself to the network as a public API node
    /// so that browsers and other clients can use it. Browsers are the
    /// reason it must be HTTPS: a page served over HTTPS cannot open a
    /// plain-HTTP connection, so a node without a certificate is
    /// unreachable from every web application regardless of how healthy
    /// it is.
    ///
    /// Omitting it is not a fault. The node works exactly as well; it just
    /// does not tell anyone it can be dialled directly, which is right for
    /// a node on a laptop or behind a firewall.
    #[arg(long, value_name = "URL")]
    pub public_rpc_url: Option<String>,

    /// How long to keep pinned content: a number of days, or `archival`.
    ///
    /// Defaults to 30 days. Not every node should store the whole history
    /// — that would make running one an open-ended commitment — so the
    /// default is a bounded window and keeping everything is a choice.
    ///
    /// 30 days is also the floor every node owes the network, so shorter
    /// values are refused rather than quietly raised. Challenges are only
    /// ever drawn from inside that floor, which is what lets a rolling
    /// node evict correctly without losing its reward share.
    ///
    /// It governs content blocks and nothing else. The gossip event log
    /// keeps a flat week whatever this says, records with an `expires_at`
    /// go when they expire, and the marketplace records — settlements,
    /// disputes, advertisements and the rest — are kept for good, because
    /// a wallet's reputation is derived by scanning them and cannot be
    /// recomputed once they are gone. See `docs/getting-started.md` for
    /// the full table and `openfiat_rpc::actor::poll_expired_records` for
    /// the reasoning per family.
    #[arg(long, value_name = "DAYS|archival", default_value = "30", value_parser = Retention::parse)]
    pub retention: Retention,

    /// Log verbosity: `error`, `warn`, `info`, `debug` or `trace`.
    ///
    /// Accepts per-module filters too, so a noisy subsystem can be turned up
    /// alone — `--log info,openfiat_rpc::actor=debug` traces chain polling
    /// and relays without the rest. Dependencies stay at `warn` unless named
    /// explicitly; libp2p and hyper at `debug` bury everything the node
    /// itself says.
    #[arg(long, value_name = "FILTER", default_value = "info")]
    pub log: String,
}

/// Sets up logging before anything else runs.
///
/// The node previously emitted six `println!`s at startup and nothing
/// afterwards: no record of whether the chain was reachable, whether a peer
/// connected, whether a relayed transaction was submitted. An operator's
/// only signal that anything was wrong was the absence of an effect.
///
/// Written to stderr so that stdout stays free for anything a future
/// subcommand may want to pipe, and so `journalctl` captures it either way.
fn init_logging(filter: &str) {
    use tracing_subscriber::{EnvFilter, fmt};

    // Dependencies default to `warn`: libp2p, hyper and reqwest at `info`
    // or below produce enough volume to hide the node's own lines, which
    // defeats the purpose of turning logging up in the first place.
    //
    // The target is `openfiat_node`, NOT `openfiat_cli`. The crate is
    // packaged as openfiat-cli but the BINARY is openfiat-node, and tracing
    // targets come from the module path of the compiled binary. Filtering
    // on the package name silently dropped every line the node itself
    // logged; only warnings slipped through via the global default, so the
    // logger looked like it worked while emitting almost nothing.
    let directives = format!(
        "warn,openfiat_node={filter},openfiat_rpc={filter},openfiat_chain={filter},openfiat_gossip={filter},openfiat_discovery={filter},openfiat_api={filter}"
    );
    let env_filter = EnvFilter::try_new(&directives).unwrap_or_else(|_| EnvFilter::new("info"));

    fmt()
        .with_env_filter(env_filter)
        .with_target(true)
        .with_writer(std::io::stderr)
        .init();
}

/// This node's identity: a Solana CLI-format wallet.json (the same file
/// `solana-keygen new` produces) at `--identity`, defaulting to
/// `<ledger>/wallet.json`.
///
/// Generating one when the file is absent is deliberate — a first run
/// should work — but it is loud, because a node that quietly invents a
/// new identity on every boot cannot hold stake: the bond binds to the
/// key, and a key that changes is a bond that vanishes.
fn load_or_generate_wallet(path: &str) -> Wallet {
    match openfiat_wallet::solana_keyfile::load(path) {
        Ok(wallet) => {
            tracing::info!(path, "loaded node identity");
            wallet
        }
        Err(err) => {
            tracing::warn!(
                path,
                %err,
                "no usable wallet — generating a throwaway identity for this run. It is NOT \
                 saved, so this node's identity, and any stake bound to it, will differ on the \
                 next start. Create one with `solana-keygen new -o <path>`."
            );
            Wallet::generate()
        }
    }
}

/// Every "gateway"-style role this node claims — see the module doc for
/// why a general-purpose node claims all of them rather than none.
fn gateway_roles() -> Vec<NodeRole> {
    vec![
        NodeRole::MerchantGateway,
        NodeRole::OracleProvider,
        NodeRole::NotificationGateway,
        NodeRole::RiskIntelligenceProvider,
    ]
}

/// `--solana-rpc-url` (repeatable) puts this node in
/// `NodeChainMode::RpcConnected` (OFS-4300 §4), submitting relayed
/// transactions to every endpoint in parallel (§7). With none given it is
/// `GossipOnly` — the safe, zero-config default; an operator opts into a
/// real RPC connection explicitly.
fn chain_mode(args: &Args) -> NodeChainMode {
    if args.solana_rpc_urls.is_empty() {
        NodeChainMode::GossipOnly
    } else {
        NodeChainMode::RpcConnected {
            rpc_urls: args.solana_rpc_urls.clone(),
            ws_url: args.solana_ws_url.clone(),
        }
    }
}

/// Snapshot production settings.
///
/// Production is on unless `--no-snapshot-production` turns it off; what
/// used to gate it — knowing this node's public URL — the node now works
/// out for itself from the addresses it learns it is reachable at (see
/// `openfiat_snapshot::reachable`).
///
/// `--public-rpc-url` doubles as the snapshot override when no explicit
/// one is given. It is the same fact: an operator who has already declared
/// where this node's HTTP API is publicly reachable has declared where its
/// snapshots are, because they are served by that same HTTP server.
///
/// All of these are operational under this module's own rule — they change
/// what this node offers others, never what it believes. The state root
/// decides that, whichever mirror the bytes came from.
fn snapshot_config(args: &Args, ledger: &str) -> openfiat_snapshot::SnapshotConfig {
    let directory = args
        .snapshot_dir
        .clone()
        .unwrap_or_else(|| format!("{ledger}/snapshots"))
        .into();
    let mut public_urls: Vec<openfiat_snapshot::SnapshotLocation> = args
        .snapshot_public_urls
        .iter()
        .map(|s| {
            openfiat_snapshot::SnapshotLocation::parse(s)
                .unwrap_or_else(|e| panic!("invalid --snapshot-public-url {s:?}: {e}"))
        })
        .collect();
    if public_urls.is_empty()
        && let Some(url) = &args.public_rpc_url
        && let Ok(location) = openfiat_snapshot::SnapshotLocation::parse(url.clone())
    {
        public_urls.push(location);
    }
    let interval = args
        .snapshot_interval_secs
        .map(std::time::Duration::from_secs)
        .unwrap_or(openfiat_snapshot::config::DEFAULT_INTERVAL);

    openfiat_snapshot::SnapshotConfig {
        directory,
        interval: (!args.no_snapshot_production).then_some(interval),
        public_urls,
        // Parsed rather than resolved: a `HOST:PORT` naming a hostname
        // binds fine but says nothing this node can derive a location
        // from, so it falls back to the operator's override — the same
        // answer as any other address it cannot reason about.
        rpc_bind: args.rpc_bind_address.parse().ok(),
        retain: openfiat_snapshot::config::DEFAULT_RETAIN,
        trusted_providers: openfiat_snapshot::trust::TrustAnchors::with_operator(
            &args.trusted_snapshot_providers,
        )
        .unwrap_or_else(|e| panic!("invalid --trusted-snapshot-provider: {e}")),
    }
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    init_logging(&args.log);

    let identity_path = args
        .identity
        .clone()
        .unwrap_or_else(|| format!("{}/wallet.json", args.ledger.trim_end_matches('/')));
    let wallet = load_or_generate_wallet(&identity_path);
    // Reusing the wallet's own seed keeps one operator identity across
    // Solana tooling and this node's gossip/network keypair (see module
    // doc) — `openfiat_crypto::Keypair` and `openfiat_wallet::Wallet`
    // both derive from the same Ed25519 seed, just via separate types
    // (`Wallet` has no accessor for its inner keypair — it's meant to
    // stay a signing primitive, not a network identity).
    let network_keypair = Keypair::from_seed(wallet.seed());

    let data_dir = args.ledger.clone();
    let http_addr = args.rpc_bind_address.clone();
    let listen_addr = args.gossip_bind_address.clone();
    // Resolved here rather than by libp2p: the `dns` transport feature is
    // deliberately not enabled (see `openfiat_network::identity`), so a
    // hostname entrypoint would otherwise fail to dial with nothing said
    // about why.
    let mut bootstrap_peers = Vec::new();
    for entrypoint in &args.entrypoints {
        match openfiat_network::identity::resolve_dns_multiaddr(entrypoint) {
            Ok(resolved) => {
                if resolved.iter().any(|a| a != entrypoint) {
                    tracing::info!(
                        %entrypoint,
                        resolved = resolved.len(),
                        "resolved entrypoint hostname"
                    );
                }
                bootstrap_peers.extend(resolved);
            }
            // Fatal rather than skipped. A node that quietly dropped an
            // unresolvable entrypoint would come up with no peers at all
            // and look perfectly healthy while talking to nobody.
            Err(err) => {
                tracing::error!(%entrypoint, %err, "--entrypoint could not be resolved");
                std::process::exit(1);
            }
        }
    }
    let chain_mode = chain_mode(&args);

    // Probe every endpoint before claiming RpcConnected.
    //
    // `solana_client` reads responses with `value["result"]`, which PANICS
    // rather than erroring when the endpoint answers with anything else —
    // and the panic kills the chain thread while the HTTP server keeps
    // serving, so the node reports itself healthy and silently stops doing
    // anything on chain. Better to refuse to start, naming the endpoint.
    for url in &args.solana_rpc_urls {
        if let Err(err) = openfiat_chain::validate_rpc_endpoint(url).await {
            tracing::error!(%err, "--solana-rpc-url is not usable");
            std::process::exit(1);
        }
    }
    let snapshot = snapshot_config(&args, &data_dir);
    let snapshot_directory = snapshot.directory.clone();

    // Both identities, in the encodings they are actually used in.
    //
    // They are the same key, and printing either as raw bytes leaves an
    // operator with a number they cannot look up, search for, or compare
    // against what their wallet shows them. The address is what an
    // explorer, a faucet and a stake instruction take; the peer id is what
    // goes in the multiaddr other operators put after `--entrypoint`, so
    // a node that cannot state its own peer id cannot be joined.
    let libp2p_peer_id =
        openfiat_network::PeerId::from_public_key(&to_libp2p_keypair(&network_keypair).public());

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        ledger = %data_dir,
        address = %wallet.address(),
        peer_id = %libp2p_peer_id,
        chain_mode = if chain_mode.is_rpc_connected() { "RpcConnected" } else { "GossipOnly" },
        // Logged, not read: which deployment a binary is pinned to is fixed
        // at compile time (`openfiat_chain::programs`), so this is an
        // operator's way to *see* it, never to change it.
        network = openfiat_chain::PROGRAM_IDS.network,
        staking_program = openfiat_chain::PROGRAM_IDS.staking,
        entrypoints = args.entrypoints.len(),
        external_addrs = args.external_addrs.len(),
        retention = %args.retention.describe(),
        content_serving = if args.no_content_serving { "off" } else { "on" },
        content_announce = if args.no_content_serving || args.no_content_announce { "off" } else { "on" },
        public_url = args.public_rpc_url.as_deref().unwrap_or("(not advertised)"),
        "starting"
    );

    // Worth its own line at WARN: a node with no entrypoints and no peer
    // discovery is isolated, which looks identical to a healthy node from
    // every local check — it serves its own state happily and simply never
    // learns anything from anyone.
    if args.entrypoints.is_empty() {
        tracing::warn!(
            "no --entrypoint given: this node will not connect to any peer. Peer discovery \
             spreads knowledge between connected nodes, but something has to make the first \
             connection."
        );
    }

    let rpc_handle = openfiat_rpc::spawn_actor(
        move || {
            Database::open(&data_dir, &column_families())
                .expect("failed to open the node's RocksDB data directory")
        },
        NetworkConfig {
            keypair: network_keypair,
            self_roles: gateway_roles(),
            listen_addr,
            bootstrap_peers,
            chain_mode,
            snapshot,
            external_addresses: args.external_addrs.clone(),
            payout_wallet: args.payout_wallet.clone(),
            region: args.region.clone(),
            branding: openfiat_rpc::ServiceBranding {
                name: args.node_name.clone(),
                description: args.node_description.clone(),
                logo: args.node_logo.clone(),
                website: args.node_website.clone(),
            },
            serve_content: !args.no_content_serving,
            announce_content: !args.no_content_announce,
            content_gateway: args.content_gateway.clone(),
            ipfs_api_url: args.ipfs_api_url.clone(),
            public_rpc_url: args.public_rpc_url.clone(),
            retention: args.retention,
        },
    );
    let metrics = Arc::new(openfiat_metrics::MetricsRegistry::new());
    let router =
        openfiat_rpc::router(rpc_handle, metrics, snapshot_directory).merge(openfiat_api::router());

    let listener = tokio::net::TcpListener::bind(&http_addr)
        .await
        .unwrap_or_else(|e| panic!("failed to bind {http_addr}: {e}"));
    tracing::info!(address = %http_addr, "JSON-RPC and HTTP API listening (try GET /health, GET /docs)");

    // Deliberately no entrypoint here: at this point there isn't one to
    // print. `--gossip-bind-address` defaults to `0.0.0.0`, which tells a
    // socket "every interface" and tells a dialing peer nothing — an
    // earlier version of this line printed it anyway, and an operator who
    // handed that string to a peer got an unexplained dial failure on the
    // far side. The real addresses are logged by `openfiat_rpc::actor` as
    // libp2p resolves them and as peers report what they observed.
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("openfiat-node HTTP server failed");
}

/// Resolves once this process is asked to stop — `systemctl stop` (SIGTERM)
/// or Ctrl+C (SIGINT) on Linux, Ctrl+C on Windows. Without this, the
/// process's own default signal disposition just kills it outright, giving
/// systemd's `TimeoutStopSec` nothing graceful to wait for and skipping
/// OFNP §23's graceful-disconnect sequence entirely.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install the Ctrl+C signal handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install the SIGTERM signal handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
    tracing::info!("shutdown signal received, closing gracefully");
}

#[cfg(test)]
mod tests {
    /// Every column family any part of this node writes to must be opened
    /// at startup.
    ///
    /// `Database::open` takes the full set up front and a `put` to one that
    /// was never opened fails — and every call site in this workspace
    /// discards that result, so the failure is silent. `peers` was missing,
    /// which meant a RocksDB node recorded no discovered peer at all and
    /// `getPeers` answered with an empty list while the node was connected
    /// to somebody. It looked like a network of one.
    ///
    /// Asserted against the names the owning crates declare, so a crate
    /// that adds or renames a family fails here rather than in production
    /// silence.
    #[test]
    fn every_column_family_the_node_writes_to_is_opened() {
        let opened = super::column_families();
        for required in [
            // openfiat_discovery::cache
            "peers",
            // openfiat_gossip's event log, and snapshot bookkeeping
            "gossip_events",
            "snapshot_metadata",
            "snapshot_checkpoint",
        ] {
            assert!(
                opened.contains(&required),
                "`{required}` is written by this node but never opened, so every \
                 write to it fails silently"
            );
        }
        for snapshotted in openfiat_rpc::SNAPSHOT_COLUMN_FAMILIES {
            assert!(
                opened.contains(snapshotted),
                "`{snapshotted}` is not opened"
            );
        }
    }

    use super::*;

    #[test]
    fn no_column_family_is_opened_twice() {
        // The two lists are maintained by different people for different
        // reasons, and a family named in both is a family RocksDB is
        // asked to open twice. `pinned_content` was in both the moment it
        // became part of a snapshot.
        let families = column_families();
        let unique: std::collections::HashSet<_> = families.iter().collect();
        assert_eq!(unique.len(), families.len(), "{families:?}");
    }

    #[test]
    fn the_content_a_node_serves_is_part_of_its_snapshotted_state() {
        // It was excluded on the grounds that a peer could refetch it
        // from IPFS, which is exactly the availability this network is
        // paid to stop assuming.
        assert!(
            openfiat_rpc::SNAPSHOT_COLUMN_FAMILIES
                .contains(&openfiat_content::CONTENT_COLUMN_FAMILY)
        );
    }
}
