//! A snapshot carries the content blocks a node serves, and an importing
//! node checks every one of them against the CID it arrived under.
//!
//! # Why the state root is not enough
//!
//! A snapshot's state root proves the blob is the one the producer
//! announced and signed. The producer computed that root over whatever
//! they assembled, so it says nothing about whether what they assembled
//! is honest. For records that is fine — a producer able to forge a
//! settlement could have gossiped one. For content blocks it is not: a
//! block is *named by the hash of its own bytes*, so a pair that
//! disagrees with itself is either corruption or an arrangement for this
//! node to serve someone else's bytes under a CID a signed attachment
//! record points at, to challengers and to browsers alike.
//!
//! These tests exercise `openfiat_rpc::state::verify_snapshot_entry`,
//! which is the function a real node hands to its `SnapshotIndex`.

use openfiat_content::{CONTENT_COLUMN_FAMILY, MAX_BLOCK_BYTES};
use openfiat_crypto::Cid;
use openfiat_rpc::state::verify_snapshot_entry;
use openfiat_snapshot::error::SnapshotError;
use openfiat_snapshot::state;
use openfiat_storage::{KvStore, mem::MemoryStore};

/// The bytes this CID names — uploaded to Filebase, fetched back from an
/// unrelated gateway.
const PROBE_CID: &str = "bafkreibdmq27skp3wnycoyoqcei47etyaulerpsegivlkfvyhjkw7ufjva";
const PROBE_CONTENT: &[u8] = b"openfiat ipfs probe 1785426891\n";

/// A snapshot blob carrying one record and whatever content is given.
fn snapshot_of(blocks: &[(&[u8], &[u8])]) -> Vec<u8> {
    let source = MemoryStore::new();
    source
        .put("advertisements", b"ad-1", b"an ordinary record")
        .unwrap();
    for (key, value) in blocks {
        source.put(CONTENT_COLUMN_FAMILY, key, value).unwrap();
    }
    state::serialize(&source, &["advertisements", CONTENT_COLUMN_FAMILY]).unwrap()
}

#[test]
fn a_snapshot_of_honest_blocks_imports_and_the_node_can_serve_them() {
    // The point of carrying content at all: a node bootstrapping from a
    // snapshot comes up able to answer for the evidence its records
    // reference, rather than refetching it for hours while it earns
    // nothing and serves nobody.
    let bytes = snapshot_of(&[(PROBE_CID.as_bytes(), PROBE_CONTENT)]);

    let target = MemoryStore::new();
    assert_eq!(
        state::restore(&target, &bytes, &[], verify_snapshot_entry),
        Ok(2)
    );
    assert_eq!(
        target
            .get(CONTENT_COLUMN_FAMILY, PROBE_CID.as_bytes())
            .unwrap(),
        Some(PROBE_CONTENT.to_vec())
    );
}

#[test]
fn a_block_that_does_not_hash_to_its_key_is_refused_and_nothing_is_written() {
    // The attack the check exists for. The CID is real and appears in
    // signed attachment records; the bytes are the producer's own. A node
    // that stored this would serve it to a challenger as the named
    // content and to a browser as a party's evidence.
    let bytes = snapshot_of(&[(PROBE_CID.as_bytes(), b"substituted by the producer")]);

    let target = MemoryStore::new();
    assert_eq!(
        state::restore(&target, &bytes, &[], verify_snapshot_entry),
        Err(SnapshotError::UnverifiableEntry)
    );
    assert_eq!(
        target
            .get(CONTENT_COLUMN_FAMILY, PROBE_CID.as_bytes())
            .unwrap(),
        None
    );
    assert_eq!(
        target.get("advertisements", b"ad-1").unwrap(),
        None,
        "the honest half of a dishonest snapshot is not imported either"
    );
}

#[test]
fn a_key_that_is_not_a_content_address_is_refused() {
    // Without this, a producer names a key of their choosing and the
    // check that a block matches its CID never runs at all.
    for key in [
        b"../../etc/passwd".as_slice(),
        b"".as_slice(),
        b"bafkrei-not-a-real-cid".as_slice(),
        &[0xff, 0xfe, 0xfd],
    ] {
        let bytes = snapshot_of(&[(key, PROBE_CONTENT)]);
        assert_eq!(
            state::restore(&MemoryStore::new(), &bytes, &[], verify_snapshot_entry),
            Err(SnapshotError::UnverifiableEntry),
            "{key:?}"
        );
    }
}

#[test]
fn a_block_larger_than_any_well_formed_one_is_refused() {
    // Matching what a node accepts from a peer over bitswap: a snapshot
    // is a peer's bytes too, and a producer must not have a looser route
    // into this node's store than a stranger on the wire.
    let oversized = vec![0u8; MAX_BLOCK_BYTES + 1];
    let cid = {
        let mut binary = vec![0x01u8, 0x55, 0x12, 0x20];
        binary.extend_from_slice(&openfiat_crypto::hash::sha256(&oversized));
        Cid::from_binary(&binary).unwrap()
    };
    // Genuinely the block its CID names, so only the size can refuse it.
    assert!(cid.matches(&oversized));

    let bytes = snapshot_of(&[(cid.as_str().as_bytes(), &oversized)]);
    assert_eq!(
        state::restore(&MemoryStore::new(), &bytes, &[], verify_snapshot_entry),
        Err(SnapshotError::UnverifiableEntry)
    );
}

#[test]
fn a_records_only_snapshot_is_unaffected_by_any_of_this() {
    // Most snapshots on most nodes. The check must not have made the
    // ordinary path stricter.
    let bytes = snapshot_of(&[]);
    assert_eq!(
        state::restore(&MemoryStore::new(), &bytes, &[], verify_snapshot_entry),
        Ok(1)
    );
}
