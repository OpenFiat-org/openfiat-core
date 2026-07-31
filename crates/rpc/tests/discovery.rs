//! Three real nodes, and the one that was never told about the third.
//!
//! Peer discovery was fully implemented, converged five nodes in its own
//! crate's test, and was constructed by no running node — so in practice a
//! cluster only worked because every node was handed every other node's
//! address statically. This is the test that would have failed then and
//! passes now: node B is given exactly one address, node A's, and ends up
//! knowing node C.
//!
//! Deliberately end to end, through `spawn_actor` and the JSON-RPC surface
//! rather than against `DiscoveryService` directly. The bug was never in
//! the service; it was that nothing drove it. A test that constructed the
//! service itself would have passed throughout.

use openfiat_rpc::actor::NetworkConfig;
use openfiat_rpc::{RpcHandle, spawn_actor};
use openfiat_storage::mem::MemoryStore;
use openfiat_types::NodeRole;
use serde_json::Value;
use std::time::Duration;

/// A UDP port nothing is currently listening on.
///
/// These tests need the port *known before the node starts* — peers are
/// given an entrypoint address in advance, which is also the real
/// deployment shape, since an entrypoint is published rather than
/// discovered. That rules out binding `:0` and reading back what the OS
/// chose, but it does not require the number to be a literal.
///
/// It used to be a literal, and the literals collided: this file failed
/// under a full `cargo test --workspace` run and passed in isolation,
/// which is the signature of a port already held by another test binary
/// or — on a machine that runs real nodes, as the development box does —
/// by an actual node. Asking the OS for a free port and immediately
/// releasing it leaves a small race, but a small race is a different
/// class of problem from a fixed number that is *guaranteed* to clash
/// with anything else that picked the same one.
fn free_udp_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .expect("the loopback interface has a free UDP port")
        .local_addr()
        .expect("a bound socket has a local address")
        .port()
}

fn start(port: u16, entrypoints: Vec<String>) -> RpcHandle {
    let mut config = NetworkConfig::for_test();
    config.listen_addr = format!("/ip4/127.0.0.1/udp/{port}/quic-v1")
        .parse()
        .expect("a loopback QUIC multiaddr");
    config.bootstrap_peers = entrypoints
        .iter()
        .map(|addr| addr.parse().expect("a peer multiaddr"))
        .collect();
    config.self_roles = vec![NodeRole::FullNode];
    // Nothing in this test wants content serving, and leaving it on would
    // have three nodes fetching from a public gateway during a unit test.
    config.serve_content = false;
    spawn_actor(MemoryStore::new, config)
}

async fn call(handle: &RpcHandle, method: &str) -> Value {
    handle
        .call(method.to_string(), serde_json::json!({}))
        .await
        .unwrap_or_else(|err| panic!("{method} failed: {err:?}"))
}

/// The addresses a node says peers should dial it at.
async fn announced(handle: &RpcHandle) -> Vec<String> {
    serde_json::from_value(call(handle, "getPeers").await["announced_addresses"].clone())
        .expect("announced_addresses is a list of strings")
}

/// Peer ids this node knows about, in the readable `12D3Koo…` form.
async fn known_peers(handle: &RpcHandle) -> Vec<String> {
    call(handle, "getPeers").await["peers"]
        .as_array()
        .expect("peers is a list")
        .iter()
        .map(|peer| peer["peer_id"].as_str().unwrap_or_default().to_string())
        .filter(|id| !id.is_empty())
        .collect()
}

/// Polls `condition` until it holds, so the test waits on the network
/// rather than on a sleep long enough to usually work.
async fn eventually(label: &str, mut condition: impl AsyncFnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if condition().await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{label} did not happen within 30s"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_node_learns_a_peer_it_was_never_given() {
    // Ports decided up front, because B and C have to be told A's address
    // before A is running and there is no side channel in this topology to
    // learn an OS-assigned one through. That is also the real deployment
    // shape: an entrypoint is a published address, not a discovered one.
    // Decided up front is not the same as hardcoded — see `free_udp_port`.
    let alpha_port = free_udp_port();
    let alpha = start(alpha_port, Vec::new());
    eventually("alpha binds a listen address", async || {
        !announced(&alpha).await.is_empty()
    })
    .await;

    let entrypoint = format!("/ip4/127.0.0.1/udp/{alpha_port}/quic-v1");
    let beta = start(free_udp_port(), vec![entrypoint.clone()]);
    let gamma = start(free_udp_port(), vec![entrypoint]);

    // Every node is now connected to alpha, and beta and gamma have never
    // heard of each other: neither was given the other's address, and
    // nothing but peer exchange can tell them.
    let gamma_id = self_peer_id(&gamma).await;

    eventually("beta learns gamma through peer exchange", async || {
        known_peers(&beta).await.contains(&gamma_id)
    })
    .await;

    // And the address it learned is one it could actually dial. A peer
    // record with an empty address list is the failure mode this whole
    // task was about: the node "knows" a peer it can never reach.
    let record = call(&beta, "getPeers").await;
    let gamma_record = record["peers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|peer| peer["peer_id"] == gamma_id.as_str())
        .expect("beta knows gamma");
    let addresses = gamma_record["addresses"].as_array().unwrap();
    assert!(
        !addresses.is_empty(),
        "a peer learned without a dialable address is not a peer that was learned"
    );
}

/// A node's own peer id, in the form peers know it by.
async fn self_peer_id(handle: &RpcHandle) -> String {
    call(handle, "getPeers").await["self_peer_id"]
        .as_str()
        .expect("a node always knows its own peer id")
        .to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_node_announces_the_address_its_operator_declared_first() {
    // The NAT case. A node's bound address is private and undialable from
    // outside; only the operator can say what the public one is, and a
    // peer trying addresses in order must get the reachable one first.
    let mut config = NetworkConfig::for_test();
    config.listen_addr = format!("/ip4/127.0.0.1/udp/{}/quic-v1", free_udp_port())
        .parse()
        .unwrap();
    config.external_addresses = vec!["/ip4/203.0.113.7/udp/4001/quic-v1".parse().unwrap()];
    config.serve_content = false;
    let node = spawn_actor(MemoryStore::new, config);

    eventually("the node binds and announces", async || {
        announced(&node).await.len() >= 2
    })
    .await;

    let addresses = announced(&node).await;
    assert_eq!(
        addresses.first().map(String::as_str),
        Some("/ip4/203.0.113.7/udp/4001/quic-v1"),
        "the declared address must come first, or a dialer times out on the private one"
    );
    assert!(
        addresses.iter().any(|a| a.contains("127.0.0.1")),
        "bound addresses are still announced — a LAN or docker peer can only reach us that way"
    );
}
