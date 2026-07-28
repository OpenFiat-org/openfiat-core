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
/// up front, unlike `MemoryStore`, which opens column families lazily.
const COLUMN_FAMILIES: &[&str] = &[
    "advertisements",
    "reservations",
    "settlements",
    "disputes",
    "identity_claims",
    "governance_proposals",
    "registry_services",
    "notification_subscriptions",
    "notification_receipts",
    "oracle_records",
    "risk_records",
    "snapshot_metadata",
    "snapshot_checkpoint",
    "sessions",
];

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
    let http_addr = std::env::var("CLI_HTTP_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    let listen_addr: Multiaddr = std::env::var("CLI_LISTEN_ADDR")
        .unwrap_or_else(|_| "/ip4/0.0.0.0/udp/4001/quic-v1".to_string())
        .parse()
        .expect("CLI_LISTEN_ADDR must be a valid multiaddr");
    let bootstrap_peers = bootstrap_peers();

    println!(
        "openfiat-node {} — data dir: {data_dir}, gossip identity: {:?}",
        env!("CARGO_PKG_VERSION"),
        wallet.peer_id(),
    );

    let rpc_handle = openfiat_rpc::spawn_actor(
        move || {
            Database::open(&data_dir, COLUMN_FAMILIES)
                .expect("failed to open the node's RocksDB data directory")
        },
        NetworkConfig {
            keypair: network_keypair,
            self_roles: gateway_roles(),
            listen_addr,
            bootstrap_peers,
        },
    );
    let metrics = Arc::new(openfiat_metrics::MetricsRegistry::new());
    let router = openfiat_rpc::router(rpc_handle, metrics).merge(openfiat_api::router());

    let listener = tokio::net::TcpListener::bind(&http_addr)
        .await
        .unwrap_or_else(|e| panic!("failed to bind {http_addr}: {e}"));
    println!("openfiat-node listening on http://{http_addr} (try GET /health, GET /docs)");
    axum::serve(listener, router)
        .await
        .expect("openfiat-node HTTP server failed");
}
