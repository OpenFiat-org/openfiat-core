//! Guards against a *new* signed payload type colliding with an existing one.
//!
//! # Why a source-scanning test
//!
//! Every signed event in this workspace is signed over the serialization of
//! its payload struct. `serde_json` emits field names but no type name, so two
//! payload structs with the same field names and types produce identical
//! bytes, and a signature made for one is valid for the other.
//!
//! That was not hypothetical. `identity::ClaimVerify` and
//! `identity::ClaimRevoke` were both `{claim_id, wallet, timestamp}` with
//! identical preconditions in `apply_verify`/`apply_revoke`, so a
//! `SignedClaimVerify` — gossiped in the clear — could be lifted by any
//! observer and replayed as a permanent revocation of the claim it had just
//! verified. `governance::ProposalWithdraw` and `ProposalActivate` collided
//! the same way. Both pairs now sign domain-separated preimages
//! (`openfiat_serialization::domain`).
//!
//! Those two fixes protect the pairs that exist today. This protects the ones
//! that do not exist yet: the collision is a property of *shape*, so it
//! reappears the moment somebody adds a third `{id, wallet, timestamp}`
//! payload — and it reappears silently, because nothing fails to compile and
//! no signature stops verifying. A unit test in either crate could not see
//! that, since the two halves may live in different crates. Reading the
//! sources is the only vantage point that can.
//!
//! # What it flags
//!
//! Any two payload types reached by a `Signed*` wrapper that share a field
//! shape, unless both already sign under distinct domain tags. It is
//! deliberately noisy in one direction: a same-shaped pair that is genuinely
//! fine still has to be acknowledged, by giving it a tag.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Payload structs that legitimately share a shape *and* already sign under
/// distinct domain tags, so the confusion is closed. Anything here must have
/// a `tag::` constant in `domain.rs` and use it on both sign and verify.
const DOMAIN_SEPARATED: &[&str] = &[
    "ClaimVerify",
    "ClaimRevoke",
    "ProposalWithdraw",
    "ProposalActivate",
];

/// Field shape of a struct: the ordered `(name, type)` pairs. Order matters
/// because `serde_json` emits fields in declaration order, so two structs
/// differing only in order do *not* collide.
type Shape = Vec<(String, String)>;

fn crate_sources(root: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // `target/` holds generated copies that would double-count.
                if path.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs")
                && let Ok(text) = fs::read_to_string(&path)
            {
                out.push((path.display().to_string(), text));
            }
        }
    }
    out
}

/// Extracts `pub struct Name { pub field: Type, ... }` bodies.
fn structs(text: &str) -> HashMap<String, Shape> {
    let mut found = HashMap::new();
    for (index, _) in text.match_indices("pub struct ") {
        let rest = &text[index + "pub struct ".len()..];
        let Some(brace) = rest.find('{') else {
            continue;
        };
        let header = &rest[..brace];
        // Skip generics and tuple structs; payload types here are plain.
        if header.contains('<') || header.contains('(') {
            continue;
        }
        let name = header.trim().to_string();
        if name.is_empty() || name.contains(char::is_whitespace) {
            continue;
        }
        let Some(end) = rest[brace..].find("\n}") else {
            continue;
        };
        let body = &rest[brace..brace + end];
        let mut shape: Shape = Vec::new();
        for line in body.lines() {
            let line = line.trim();
            let Some(decl) = line.strip_prefix("pub ") else {
                continue;
            };
            let Some((field, ty)) = decl.split_once(':') else {
                continue;
            };
            if field.contains(char::is_whitespace) {
                continue;
            }
            shape.push((
                field.trim().to_string(),
                ty.trim().trim_end_matches(',').to_string(),
            ));
        }
        if !shape.is_empty() {
            found.insert(name, shape);
        }
    }
    found
}

#[test]
fn no_two_signed_payload_types_share_a_shape() {
    let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/serialization has a parent");
    let sources = crate_sources(crates_dir);
    assert!(
        sources.len() > 100,
        "expected to scan the whole workspace, saw {} files",
        sources.len()
    );

    // Every struct in the workspace, and every payload named by a `Signed*`
    // wrapper's single non-signature field.
    let mut all: HashMap<String, Shape> = HashMap::new();
    let mut payload_types: Vec<String> = Vec::new();
    for (_, text) in &sources {
        let found = structs(text);
        for (name, shape) in &found {
            if let Some(inner) = name.strip_prefix("Signed") {
                let _ = inner;
                for (_, ty) in shape {
                    let ty = ty.trim();
                    if ty != "Signature" && !ty.is_empty() {
                        payload_types.push(ty.to_string());
                    }
                }
            }
        }
        all.extend(found);
    }
    payload_types.sort();
    payload_types.dedup();
    assert!(
        payload_types.len() > 15,
        "expected to find the workspace's signed payload types, found {payload_types:?}"
    );

    let mut by_shape: HashMap<Shape, Vec<String>> = HashMap::new();
    for name in &payload_types {
        if let Some(shape) = all.get(name) {
            by_shape
                .entry(shape.clone())
                .or_default()
                .push(name.clone());
        }
    }

    let mut offenders = Vec::new();
    for (shape, mut names) in by_shape {
        if names.len() < 2 {
            continue;
        }
        names.sort();
        if names.iter().all(|n| DOMAIN_SEPARATED.contains(&n.as_str())) {
            continue;
        }
        offenders.push(format!(
            "  {names:?} all serialize as {:?}",
            shape.iter().map(|(f, _)| f).collect::<Vec<_>>()
        ));
    }
    offenders.sort();

    assert!(
        offenders.is_empty(),
        "signed payload types share a field shape, so a signature for one is a valid \
         signature for another. Give each a domain tag in \
         openfiat_serialization::domain and use it on both sign and verify, then add \
         them to DOMAIN_SEPARATED here:\n{}",
        offenders.join("\n")
    );
}
