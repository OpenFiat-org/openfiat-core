//! A node can be given content, not only asked for it.
//!
//! Holding and serving were built first, and there was no way to put
//! bytes in — so an interface that wanted to store an avatar or a piece
//! of dispute evidence had to reach a third-party pinning service, and a
//! deployment without one told the user uploads were unavailable while a
//! node perfectly capable of holding the bytes sat behind it.
//!
//! The check that makes an open ingress safe is that a caller chooses
//! *what* to store and never *where*: the CID is recomputed from the
//! bytes, so nothing can be stored under a CID it does not hash to, and
//! no existing content can be replaced with different content.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use openfiat_crypto::Cid;
use openfiat_rpc::methods::build_table;
use openfiat_rpc::state::NodeState;
use openfiat_storage::mem::MemoryStore;
use serde_json::{Value, json};

const PROBE_CID: &str = "bafkreibdmq27skp3wnycoyoqcei47etyaulerpsegivlkfvyhjkw7ufjva";
const PROBE_CONTENT: &[u8] = b"openfiat ipfs probe 1785426891\n";
const OTHER_CID: &str = "bafkreibqyjcrlslvz3uen3qjl6gaqyxu2tryyvqlb555rluyyszpg5zbqu";
const OTHER_CONTENT: &[u8] = b"a second attachment";

#[test]
fn content_put_into_a_node_comes_back_out_of_it() {
    let table = build_table::<MemoryStore>();
    let state = NodeState::new_for_test(MemoryStore::new());
    let cid = Cid::parse(PROBE_CID).unwrap();

    let put: Value = table
        .dispatch(
            &state,
            "sendContentPut",
            json!({ "cid": PROBE_CID, "content": BASE64.encode(PROBE_CONTENT) }),
        )
        .expect("a node that serves content can be given some");
    assert_eq!(put["cid"], PROBE_CID);
    assert_eq!(put["stored"], true);

    let held: Value = table
        .dispatch(&state, "getHeldContent", json!({ "cid": PROBE_CID }))
        .unwrap();
    let bytes = BASE64.decode(held["content"].as_str().unwrap()).unwrap();
    assert!(
        cid.matches(&bytes),
        "what came back is not what the CID names"
    );
    assert_eq!(bytes, PROBE_CONTENT);
}

/// The whole security of the ingress. Without this a caller could store
/// bytes of their choosing under a CID of their choosing, and every
/// content address on this node would mean whatever the last uploader
/// said it meant.
#[test]
fn bytes_that_do_not_hash_to_the_cid_are_refused() {
    let table = build_table::<MemoryStore>();
    let state = NodeState::new_for_test(MemoryStore::new());

    let refused = table.dispatch(
        &state,
        "sendContentPut",
        json!({ "cid": PROBE_CID, "content": BASE64.encode(OTHER_CONTENT) }),
    );
    assert!(
        refused.is_err(),
        "a CID/content mismatch must not be stored"
    );

    let held: Value = table
        .dispatch(&state, "getHeldContent", json!({ "cid": PROBE_CID }))
        .unwrap();
    assert!(
        held["content"].is_null(),
        "the refused bytes must not be readable under the CID they claimed"
    );
}

/// Following from the same property: an uploader cannot displace content
/// somebody else already stored, because the only CID their bytes can
/// occupy is the one those bytes hash to.
#[test]
fn an_upload_cannot_replace_content_already_held() {
    let table = build_table::<MemoryStore>();
    let state = NodeState::new_for_test(MemoryStore::new());

    for (cid, content) in [(PROBE_CID, PROBE_CONTENT), (OTHER_CID, OTHER_CONTENT)] {
        table
            .dispatch(
                &state,
                "sendContentPut",
                json!({ "cid": cid, "content": BASE64.encode(content) }),
            )
            .unwrap();
    }

    // Try to put OTHER's bytes under PROBE's name, now that both exist.
    let refused = table.dispatch(
        &state,
        "sendContentPut",
        json!({ "cid": PROBE_CID, "content": BASE64.encode(OTHER_CONTENT) }),
    );
    assert!(refused.is_err());

    let held: Value = table
        .dispatch(&state, "getHeldContent", json!({ "cid": PROBE_CID }))
        .unwrap();
    let bytes = BASE64.decode(held["content"].as_str().unwrap()).unwrap();
    assert_eq!(bytes, PROBE_CONTENT, "the original content was displaced");
}

/// An interface that retries a failed request must not be told the second
/// attempt was a failure. Re-uploading is a success with `stored: false`.
#[test]
fn re_uploading_the_same_content_succeeds_and_says_it_was_already_here() {
    let table = build_table::<MemoryStore>();
    let state = NodeState::new_for_test(MemoryStore::new());
    let params = json!({ "cid": PROBE_CID, "content": BASE64.encode(PROBE_CONTENT) });

    let first: Value = table
        .dispatch(&state, "sendContentPut", params.clone())
        .unwrap();
    let second: Value = table.dispatch(&state, "sendContentPut", params).unwrap();

    assert_eq!(first["stored"], true);
    assert_eq!(second["stored"], false);
    assert_eq!(second["cid"], PROBE_CID);
}

#[test]
fn a_cid_that_is_not_a_cid_is_rejected_before_anything_is_stored() {
    let table = build_table::<MemoryStore>();
    let state = NodeState::new_for_test(MemoryStore::new());

    for bad in ["", "not-a-cid", "../../etc/passwd"] {
        assert!(
            table
                .dispatch(
                    &state,
                    "sendContentPut",
                    json!({ "cid": bad, "content": BASE64.encode(PROBE_CONTENT) }),
                )
                .is_err(),
            "{bad} was accepted as a content address"
        );
    }
}
