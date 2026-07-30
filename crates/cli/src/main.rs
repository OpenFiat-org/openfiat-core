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
//! Every `CLI_*` variable this module reads is **operational** — where
//! this node listens, where it keeps its data, which peers and RPC
//! endpoints it talks to. None of them can change what this node
//! believes. Protocol identity (program ids, the OPEN mint, PDA seeds,
//! account discriminators) is pinned at compile time in
//! `openfiat_chain::programs` and is deliberately unreachable from the
//! environment; protocol parameters (fees, stake minimums, quorum) live
//! on chain under governance control. The rule those three cases follow:
//! if two honest nodes running the same release could disagree because of
//! a value, that value is not configuration.

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

/// This node's identity: a Solana CLI-format wallet.json at
/// `CLI_WALLET_PATH` (defaulting to Solana CLI's own convention,
/// `~/.config/solana/id.json`), so an operator authenticates this node
/// with the same wallet they already use for Solana tooling.
fn load_or_generate_wallet() -> Wallet {
    let path = std::env::var("CLI_WALLET_PATH").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        format!("{home}/.config/solana/id.json")
    });

    match openfiat_wallet::solana_keyfile::load(&path) {
        Ok(wallet) => {
            println!("openfiat-node: loaded node identity from {path}");
            wallet
        }
        Err(err) => {
            eprintln!(
                "openfiat-node: no usable wallet at {path} ({err}), generating a fresh identity for this run"
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

/// `CLI_BOOTSTRAP_PEERS`: a comma-separated multiaddr list of peers to
/// dial on startup, e.g. `/ip4/10.0.0.2/udp/4001/quic-v1`. Empty by
/// default — a lone bootstrap node has nothing to dial.
fn bootstrap_peers() -> Vec<Multiaddr> {
    std::env::var("CLI_BOOTSTRAP_PEERS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse()
                .unwrap_or_else(|e| panic!("invalid CLI_BOOTSTRAP_PEERS entry {s:?}: {e}"))
        })
        .collect()
}

/// `CLI_SOLANA_RPC_URLS` (comma-separated, e.g.
/// `https://api.devnet.solana.com,https://your-provider/?api-key=...`)
/// puts this node in `NodeChainMode::RpcConnected` (OFS-4300 §4),
/// submitting relayed transactions to every configured endpoint in
/// parallel (§7). Unset (the default) is `GossipOnly` — the safe,
/// zero-config choice; an operator opts into a real RPC connection
/// explicitly. `CLI_SOLANA_WS_URL` is optional and only recorded on the
/// mode today — no subscription-based polling reads it yet.
fn chain_mode() -> NodeChainMode {
    let rpc_urls: Vec<String> = std::env::var("CLI_SOLANA_RPC_URLS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();

    if rpc_urls.is_empty() {
        NodeChainMode::GossipOnly
    } else {
        NodeChainMode::RpcConnected {
            rpc_urls,
            ws_url: std::env::var("CLI_SOLANA_WS_URL").ok(),
        }
    }
}

/// `CLI_SNAPSHOT_DIR` (default `{CLI_DATA_DIR}/snapshots`) is where this
/// node writes the snapshots it produces and the only directory it serves
/// `GET /snapshot/{id}` from.
///
/// `CLI_SNAPSHOT_PUBLIC_URLS` (comma-separated, e.g.
/// `https://archive.example`) is what this node tells the cluster to
/// fetch those snapshots from. A node cannot work this out for itself —
/// it sees a bind address, not what a proxy or NAT makes of it — so
/// leaving it unset disables snapshot *production* entirely. Consuming
/// snapshots needs no configuration at all: a node with no checkpoint
/// bootstraps from whatever the cluster has announced.
///
/// `CLI_SNAPSHOT_INTERVAL_SECS` overrides the production cadence.
///
/// All three are operational under this module's own rule: they change
/// what this node offers others, never what it believes. The state root
/// decides that, whichever mirror the bytes came from.
fn snapshot_config(data_dir: &str) -> openfiat_snapshot::SnapshotConfig {
    let directory = std::env::var("CLI_SNAPSHOT_DIR")
        .unwrap_or_else(|_| format!("{data_dir}/snapshots"))
        .into();
    let public_urls: Vec<openfiat_snapshot::SnapshotLocation> =
        std::env::var("CLI_SNAPSHOT_PUBLIC_URLS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| {
                openfiat_snapshot::SnapshotLocation::parse(s)
                    .unwrap_or_else(|e| panic!("invalid CLI_SNAPSHOT_PUBLIC_URLS entry {s:?}: {e}"))
            })
            .collect();
    let interval = std::env::var("CLI_SNAPSHOT_INTERVAL_SECS")
        .ok()
        .map(|raw| {
            raw.parse()
                .unwrap_or_else(|e| panic!("invalid CLI_SNAPSHOT_INTERVAL_SECS {raw:?}: {e}"))
        })
        .map(std::time::Duration::from_secs)
        .unwrap_or(openfiat_snapshot::config::DEFAULT_INTERVAL);

    openfiat_snapshot::SnapshotConfig {
        directory,
        // The URL, not the interval, is what an operator opts in with:
        // an interval without one would produce snapshots nobody can
        // fetch. See `SnapshotConfig::produces`.
        interval: (!public_urls.is_empty()).then_some(interval),
        public_urls,
        retain: openfiat_snapshot::config::DEFAULT_RETAIN,
    }
}

#[tokio::main]
async fn main() {
    let wallet = load_or_generate_wallet();
    // Reusing the wallet's own seed keeps one operator identity across
    // Solana tooling and this node's gossip/network keypair (see module
    // doc) — `openfiat_crypto::Keypair` and `openfiat_wallet::Wallet`
    // both derive from the same Ed25519 seed, just via separate types
    // (`Wallet` has no accessor for its inner keypair — it's meant to
    // stay a signing primitive, not a network identity).
    let network_keypair = Keypair::from_seed(wallet.seed());

    let data_dir = std::env::var("CLI_DATA_DIR").unwrap_or_else(|_| "./openfiat-data".to_string());
    // 7080 rather than the more obvious 8080: that port is crowded on a
    // typical server (proxies, app servers, other containers all default
    // to it), and a node silently failing to bind because something else
    // got there first is a bad first five minutes.
    let http_addr = std::env::var("CLI_HTTP_ADDR").unwrap_or_else(|_| "0.0.0.0:7080".to_string());
    let listen_addr: Multiaddr = std::env::var("CLI_LISTEN_ADDR")
        .unwrap_or_else(|_| "/ip4/0.0.0.0/udp/4001/quic-v1".to_string())
        .parse()
        .expect("CLI_LISTEN_ADDR must be a valid multiaddr");
    let bootstrap_peers = bootstrap_peers();
    let chain_mode = chain_mode();
    let snapshot = snapshot_config(&data_dir);
    let snapshot_directory = snapshot.directory.clone();

    println!(
        "openfiat-node {} — data dir: {data_dir}, gossip identity: {:?}, chain mode: {}, \
         programs: {} (staking {})",
        env!("CARGO_PKG_VERSION"),
        wallet.peer_id(),
        if chain_mode.is_rpc_connected() {
            "RpcConnected"
        } else {
            "GossipOnly"
        },
        // Printed, not read: which deployment a binary is pinned to is
        // fixed at compile time (`openfiat_chain::programs`), so this is
        // an operator's way to *see* it, never to change it.
        openfiat_chain::PROGRAM_IDS.network,
        openfiat_chain::PROGRAM_IDS.staking,
    );

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
    println!("openfiat-node listening on http://{http_addr} (try GET /health, GET /docs)");
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
    println!("openfiat-node: shutdown signal received, closing gracefully");
}
