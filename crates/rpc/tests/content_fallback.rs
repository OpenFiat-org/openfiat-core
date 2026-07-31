//! An interface can retrieve an attachment from a node when the gateway
//! it normally reads through cannot serve it — and can prove the bytes
//! are the right ones without trusting the node.
//!
//! This is the durability guarantee arriving somewhere a user can feel
//! it. A node holding evidence that nobody can fetch is a node doing the
//! work and delivering nothing; `getHeldContent` is the route, and these
//! tests walk it exactly as a browser would, over the real dispatch
//! table, with every block checked by hashing.
//!
//! The chunked case is the one that matters. A CID over 256 KiB names a
//! dag-pb root whose digest covers the DAG node rather than the file, so
//! a client cannot ask for "the file" and check it. It asks for blocks,
//! and each block is named by the hash of its own bytes.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use openfiat_crypto::Cid;
use openfiat_rpc::dispatch::MethodTable;
use openfiat_rpc::methods::build_table;
use openfiat_rpc::state::NodeState;
use openfiat_storage::mem::MemoryStore;
use serde_json::{Value, json};

/// The bytes `PROBE_CID` names: uploaded to Filebase, fetched back from
/// an unrelated gateway, reproduced by an independent CID implementation.
const PROBE_CID: &str = "bafkreibdmq27skp3wnycoyoqcei47etyaulerpsegivlkfvyhjkw7ufjva";
const PROBE_CONTENT: &[u8] = b"openfiat ipfs probe 1785426891\n";

const OTHER_CID: &str = "bafkreibqyjcrlslvz3uen3qjl6gaqyxu2tryyvqlb555rluyyszpg5zbqu";
const OTHER_CONTENT: &[u8] = b"a second attachment";

/// What a client does with an answer: hash it against the CID it asked
/// for, and refuse it otherwise. Everything below goes through here,
/// because a test that unwrapped the bytes and used them would be
/// asserting the transport rather than the guarantee.
fn verified(
    table: &MethodTable<MemoryStore>,
    state: &NodeState<MemoryStore>,
    cid: &Cid,
) -> Vec<u8> {
    let answer: Value = table
        .dispatch(state, "getHeldContent", json!({ "cid": cid.as_str() }))
        .expect("getHeldContent answers for any well-formed CID");
    let encoded = answer["content"]
        .as_str()
        .expect("this node was set up holding the block");
    let bytes = BASE64.decode(encoded).expect("the answer is base64");
    assert!(
        cid.matches(&bytes),
        "bytes that do not hash to the CID asked for must never be used"
    );
    bytes
}

/// A dag-pb node linking to `children`, assembled from the wire format
/// rather than through this workspace's reader — a client writes its own,
/// and this test is standing in for one.
fn dag_pb_node(children: &[&Cid]) -> Vec<u8> {
    let mut out = Vec::new();
    for child in children {
        let hash = child.to_binary();
        let mut link = vec![0x0a, hash.len() as u8]; // PBLink field 1, Hash
        link.extend_from_slice(&hash);
        out.push(0x12); // PBNode field 2, Links
        out.push(link.len() as u8);
        out.extend_from_slice(&link);
    }
    out.extend_from_slice(&[0x0a, 0x02, 0x08, 0x02]); // Data: unixfs File
    out
}

fn dag_pb_cid(block: &[u8]) -> Cid {
    let mut binary = vec![0x01, 0x70, 0x12, 0x20];
    binary.extend_from_slice(&openfiat_crypto::hash::sha256(block));
    Cid::from_binary(&binary).expect("a dag-pb sha2-256 CID")
}

/// The links of a dag-pb node, read the way a browser would have to.
///
/// Thirty lines in any language, which is the point: the client does not
/// need an IPFS implementation to walk a file trustlessly, and it must
/// not take a link list from anywhere but a block it has already hashed.
fn links_of(block: &[u8]) -> Vec<Cid> {
    let mut links = Vec::new();
    let mut rest = block;
    while !rest.is_empty() {
        let key = rest[0];
        rest = &rest[1..];
        let length = rest[0] as usize;
        rest = &rest[1..];
        let (field, body) = (key >> 3, &rest[..length]);
        rest = &rest[length..];
        if field == 2 {
            // PBLink: field 1 is the Hash, as a binary CID.
            assert_eq!(body[0], 0x0a);
            let hash = &body[2..2 + body[1] as usize];
            links.push(Cid::from_binary(hash).expect("a link this protocol can address"));
        }
    }
    links
}

#[test]
fn a_client_can_fetch_and_check_a_small_attachment_the_gateway_would_not_serve() {
    let state = NodeState::new_for_test(MemoryStore::new());
    let cid = Cid::parse(PROBE_CID).unwrap();
    assert!(state.held_content.keep(&cid, PROBE_CONTENT));

    assert_eq!(verified(&build_table(), &state, &cid), PROBE_CONTENT);
}

#[test]
fn a_client_can_walk_a_chunked_attachment_block_by_block() {
    // The case that did not exist before: an attachment over 256 KiB is a
    // root and its leaves, and the file comes back as the leaves in the
    // order the root lists them. Every block on the way is checked
    // against a CID the client either brought with it or read out of a
    // block it had already checked.
    let leaves = [
        Cid::parse(PROBE_CID).unwrap(),
        Cid::parse(OTHER_CID).unwrap(),
    ];
    let root_block = dag_pb_node(&[&leaves[0], &leaves[1]]);
    let root = dag_pb_cid(&root_block);

    let state = NodeState::new_for_test(MemoryStore::new());
    assert!(state.held_content.keep(&root, &root_block));
    assert!(state.held_content.keep(&leaves[0], PROBE_CONTENT));
    assert!(state.held_content.keep(&leaves[1], OTHER_CONTENT));

    let table = build_table();
    let fetched_root = verified(&table, &state, &root);
    let mut file = Vec::new();
    for link in links_of(&fetched_root) {
        file.extend_from_slice(&verified(&table, &state, &link));
    }

    let mut expected = PROBE_CONTENT.to_vec();
    expected.extend_from_slice(OTHER_CONTENT);
    assert_eq!(file, expected);
}

#[test]
fn a_node_that_does_not_hold_the_block_says_so_rather_than_erroring() {
    // A client has to be able to move on to the next access node. An
    // error would be indistinguishable from a broken request, and a
    // client that treated "not here" as fatal would stop looking.
    let state = NodeState::new_for_test(MemoryStore::new());
    let answer: Value = build_table()
        .dispatch(&state, "getHeldContent", json!({ "cid": PROBE_CID }))
        .expect("an absent block is an ordinary answer");
    assert_eq!(answer["content"], Value::Null);
}
