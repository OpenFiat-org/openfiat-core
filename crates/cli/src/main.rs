//! `openfiat-node` — composition root for the OpenFiat reference node.
//!
//! This binary wires together the workspace crates. It currently only
//! prints version information for each linked crate; startup, config
//! loading, and service wiring will be added during implementation.

use openfiat_api as _;
use openfiat_config as _;
use openfiat_database as _;
use openfiat_metrics as _;
use openfiat_network as _;
use openfiat_rpc as _;

fn main() {
    println!("openfiat-node {}", env!("CARGO_PKG_VERSION"));
    println!("config:  {}", openfiat_config::version());
    println!("network: {}", openfiat_network::version());
    println!("rpc:     {}", openfiat_rpc::version());
    println!("api:     {}", openfiat_api::version());
    println!("metrics: {}", openfiat_metrics::version());
    println!("database:{}", openfiat_database::version());
}
