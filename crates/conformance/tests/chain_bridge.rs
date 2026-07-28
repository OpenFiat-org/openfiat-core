//! OFS-4300 conformance: a gossip-only node's transaction relay request
//! reaches an RPC-connected peer and its confirmation echoes back, and
//! blockhash announcement dedup bounds gossip amplification under many
//! independent announcers — see `CONFORMANCE.md`.

use openfiat_conformance::{drive_until, spawn_cluster};
use openfiat_storage::mem::MemoryStore;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

#[tokio::test]
async fn a_gossip_only_nodes_relay_request_is_observed_and_confirmed_by_an_rpc_connected_peer() {
    // Node 0 (hub) plays the RPC-connected relayer; node 1 is gossip-only
    // and has no way to submit anything itself.
    let roles = vec![vec![], vec![]];
    let mut nodes = spawn_cluster(MemoryStore::new, &roles).await;

    let submitted: Rc<RefCell<Vec<Vec<u8>>>> = Rc::new(RefCell::new(Vec::new()));
    let handler_submitted = Rc::clone(&submitted);
    nodes[0].chain.on_relay_requested(move |request| {
        handler_submitted
            .borrow_mut()
            .push(request.tx_bytes.clone());
    });

    let confirmed: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let handler_confirmed = Rc::clone(&confirmed);
    nodes[1].chain.on_relay_confirmed(move |relayed| {
        handler_confirmed
            .borrow_mut()
            .push(relayed.signature.clone());
    });

    nodes[1]
        .request_transaction_relay(b"a-signed-solana-transaction".to_vec())
        .unwrap();
    drive_until(&mut nodes, |_nodes| !submitted.borrow().is_empty()).await;
    assert_eq!(submitted.borrow()[0], b"a-signed-solana-transaction");

    // Node 0 "submits" it (out of scope of this crate — a real
    // RpcConnected node would call crates/chain's RpcChainClient here)
    // and echoes back a confirmation once it observes the result.
    nodes[0]
        .announce_relay_confirmation("5xY...onchainSig", 12345)
        .unwrap();
    drive_until(&mut nodes, |_nodes| !confirmed.borrow().is_empty()).await;
    assert_eq!(confirmed.borrow()[0], "5xY...onchainSig");
}

#[tokio::test]
async fn blockhash_dedup_bounds_amplification_under_many_independent_announcers() {
    // 10 announcer leaves + 1 pure observer leaf, all connected only to
    // the hub (node 0) — the star topology `spawn_cluster` already
    // builds. Each announcer independently originates the *same*
    // (blockhash, slot) — a distinct, separately signed event per
    // origin, exactly OFS-4300 §6's "thousands of independent
    // RPC-connected nodes" scenario at a testable scale.
    const ANNOUNCERS: usize = 10;
    let roles = vec![vec![]; ANNOUNCERS + 2]; // hub + announcers + one observer
    let mut nodes = spawn_cluster(MemoryStore::new, &roles).await;
    let observer = nodes.len() - 1;

    for node in nodes.iter_mut().skip(1).take(ANNOUNCERS) {
        node.announce_blockhash("hash-mass-announced", 500).unwrap();
    }

    drive_until(&mut nodes, |nodes| {
        nodes[0].gossip.event_count() >= ANNOUNCERS
    })
    .await;
    // Give the observer every chance to receive more than one relayed
    // copy before asserting its absence.
    let _ = tokio::time::timeout(Duration::from_millis(500), async {
        loop {
            let futures: Vec<_> = nodes.iter_mut().map(|n| Box::pin(n.drive_once())).collect();
            futures::future::select_all(futures).await;
        }
    })
    .await;

    assert_eq!(
        nodes[0].gossip.event_count(),
        ANNOUNCERS,
        "the hub stores every distinct announcement — content dedup only governs re-forwarding, not local storage"
    );
    assert_eq!(
        nodes[observer].gossip.event_count(),
        1,
        "the observer, connected only through the hub, must receive exactly one copy of the shared content \
         regardless of how many independent nodes announced it"
    );
    assert_eq!(
        nodes[observer].current_blockhash(),
        Some(("hash-mass-announced".to_string(), 500))
    );
}
