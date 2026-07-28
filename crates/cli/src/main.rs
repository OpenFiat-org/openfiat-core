//! `openfiat-node` — composition root wiring the crates above into one
//! running node: a real RocksDB-backed store, every domain registry
//! `openfiat-rpc`'s `NodeState` composes, and the merged `rpc`+`api`
//! axum server bound to a real HTTP port.
//!
//! Node identity is a Solana CLI-format wallet.json (the same file
//! `solana-keygen new` produces), the same convention
//! `openfiat-apps/explorer/indexer` already uses — see
//! `load_or_generate_wallet` below.
//!
//! This binary does not yet run real libp2p/gossip networking:
//! `openfiat-rpc`'s `sendX` handlers apply a caller's signed payload
//! straight to the local registry (see `crates/rpc/src/state.rs`'s own
//! doc comment) rather than originating it over `openfiat-gossip` for
//! other nodes to pick up. That's an intentional, already-documented
//! scope boundary of the RPC layer, not something newly introduced
//! here — multi-node propagation of RPC-submitted writes is a real,
//! separately-scoped follow-up, not a hidden gap.

use openfiat_database::Database;
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

#[tokio::main]
async fn main() {
    let _ = load_or_generate_wallet(); // TODO: node identity isn't consumed yet — NodeState has no gossip/network identity to attach it to (see module doc). Loaded now so CLI_WALLET_PATH is already the operative env var once that lands.

    let data_dir = std::env::var("CLI_DATA_DIR").unwrap_or_else(|_| "./openfiat-data".to_string());
    let http_addr = std::env::var("CLI_HTTP_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());

    println!(
        "openfiat-node {} — data dir: {data_dir}",
        env!("CARGO_PKG_VERSION")
    );

    let rpc_handle = openfiat_rpc::spawn_actor(move || {
        Database::open(&data_dir, COLUMN_FAMILIES)
            .expect("failed to open the node's RocksDB data directory")
    });
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
