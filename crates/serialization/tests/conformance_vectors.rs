//! Cross-language conformance test for the F-01 domain-separation header.
//!
//! `tests/vectors/client_signed_v1.json` is the frozen cross-repo contract:
//! the Rust SDK (via this crate), the TypeScript SDK, and the app each read
//! the *same* file and assert their own `preimage` implementation reproduces
//! every row. `payload_json` is treated as opaque bytes here — this test
//! proves the `len(tag):u32be ‖ tag ‖ body` header is byte-identical
//! regardless of which language produced `body`, independent of any struct
//! definition on either side.
//!
//! If this test fails after an intentional tag change, the vector file must
//! be regenerated *and* the same change must land in `openfiat-sdks` (TS
//! SDK) and `openfiat-app` in the same release — see `domain.rs`'s module
//! doc and the F-01 design doc.

use openfiat_serialization::domain::preimage_raw;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Vector {
    tag: String,
    payload_json: String,
    preimage_hex: String,
}

fn vectors() -> Vec<Vector> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/vectors/client_signed_v1.json"
    );
    let text =
        std::fs::read_to_string(path).expect("client_signed_v1.json must exist and be readable");
    serde_json::from_str(&text).expect("client_signed_v1.json must be a JSON array of vectors")
}

#[test]
fn every_vector_reproduces_its_preimage() {
    let vectors = vectors();
    assert!(
        vectors.len() >= 30,
        "expected the full F-01 client-signed vector set, found {}",
        vectors.len()
    );

    let mut failures = Vec::new();
    for vector in &vectors {
        let actual = hex::encode(preimage_raw(&vector.tag, vector.payload_json.as_bytes()));
        if actual != vector.preimage_hex {
            failures.push(format!(
                "tag {:?}: expected {}, got {}",
                vector.tag, vector.preimage_hex, actual
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "preimage_raw diverged from the frozen conformance vectors:\n{}",
        failures.join("\n")
    );
}

#[test]
fn every_vector_has_a_unique_tag() {
    // Not a hard requirement of preimage_raw itself, but a sanity check on
    // the fixture: a duplicate tag row would silently test less than it
    // looks like it tests.
    let vectors = vectors();
    let mut tags: Vec<&str> = vectors.iter().map(|v| v.tag.as_str()).collect();
    tags.sort_unstable();
    let mut deduped = tags.clone();
    deduped.dedup();
    assert_eq!(
        tags.len(),
        deduped.len(),
        "client_signed_v1.json has a duplicate tag"
    );
}
