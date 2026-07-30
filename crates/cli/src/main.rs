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
use openfiat_crypto::Keypair;
use openfiat_database::Database;
use openfiat_network::Multiaddr;
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
const NODE_LOCAL_COLUMN_FAMILIES: &[&str] =
    &["gossip_events", "snapshot_metadata", "snapshot_checkpoint"];

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
    /// A lone node has nothing to dial. Peer discovery does not yet run, so
    /// a node currently finds only the peers named here.
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

    /// Public URL other nodes should fetch this node's snapshots from.
    /// Repeat for several.
    ///
    /// A node cannot work this out for itself — it sees a bind address, not
    /// what a proxy or NAT makes of it — so omitting this disables snapshot
    /// PRODUCTION entirely. Consuming snapshots needs no configuration.
    #[arg(long = "snapshot-public-url", value_name = "URL")]
    pub snapshot_public_urls: Vec<String>,

    /// Seconds between snapshots. Ignored without --snapshot-public-url,
    /// since an interval alone would produce snapshots nobody can fetch.
    #[arg(long, value_name = "SECS")]
    pub snapshot_interval_secs: Option<u64>,

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
/// `--snapshot-public-url` is what an operator opts in with, not the
/// interval: an interval without a URL would produce snapshots nobody can
/// fetch. See `SnapshotConfig::produces`.
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
    let public_urls: Vec<openfiat_snapshot::SnapshotLocation> = args
        .snapshot_public_urls
        .iter()
        .map(|s| {
            openfiat_snapshot::SnapshotLocation::parse(s)
                .unwrap_or_else(|e| panic!("invalid --snapshot-public-url {s:?}: {e}"))
        })
        .collect();
    let interval = args
        .snapshot_interval_secs
        .map(std::time::Duration::from_secs)
        .unwrap_or(openfiat_snapshot::config::DEFAULT_INTERVAL);

    openfiat_snapshot::SnapshotConfig {
        directory,
        interval: (!public_urls.is_empty()).then_some(interval),
        public_urls,
        retain: openfiat_snapshot::config::DEFAULT_RETAIN,
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
    let bootstrap_peers = args.entrypoints.clone();
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

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        ledger = %data_dir,
        identity = ?wallet.peer_id(),
        chain_mode = if chain_mode.is_rpc_connected() { "RpcConnected" } else { "GossipOnly" },
        // Logged, not read: which deployment a binary is pinned to is fixed
        // at compile time (`openfiat_chain::programs`), so this is an
        // operator's way to *see* it, never to change it.
        network = openfiat_chain::PROGRAM_IDS.network,
        staking_program = openfiat_chain::PROGRAM_IDS.staking,
        entrypoints = args.entrypoints.len(),
        "starting"
    );

    // Worth its own line at WARN: a node with no entrypoints and no peer
    // discovery is isolated, which looks identical to a healthy node from
    // every local check — it serves its own state happily and simply never
    // learns anything from anyone.
    if args.entrypoints.is_empty() {
        tracing::warn!(
            "no --entrypoint given: this node will not connect to any peer. Peer discovery \
             does not run yet, so entrypoints are currently the only way to join a cluster."
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
        },
    );
    let metrics = Arc::new(openfiat_metrics::MetricsRegistry::new());
    let router =
        openfiat_rpc::router(rpc_handle, metrics, snapshot_directory).merge(openfiat_api::router());

    let listener = tokio::net::TcpListener::bind(&http_addr)
        .await
        .unwrap_or_else(|e| panic!("failed to bind {http_addr}: {e}"));
    tracing::info!(address = %http_addr, "JSON-RPC and HTTP API listening (try GET /health, GET /docs)");
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
