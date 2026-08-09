//! Guards against a *new* signed payload type colliding with an existing one,
//! and against a *new* `Signed*` type being added without ever being triaged
//! for a domain tag.
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
//! Those two fixes protect the pairs that exist today. The shape check below
//! protects the ones that do not exist yet: the collision is a property of
//! *shape*, so it reappears the moment somebody adds a third
//! `{id, wallet, timestamp}` payload — and it reappears silently, because
//! nothing fails to compile and no signature stops verifying. A unit test in
//! either crate could not see that, since the two halves may live in
//! different crates. Reading the sources is the only vantage point that can.
//!
//! # F-01: why this got a second guard, not a replacement
//!
//! As of F-01 (2026-08), `openfiat_serialization::domain::tag` names a
//! `/v1` tag for every `Signed*` type in the workspace (see `domain.rs`'s
//! module doc) — but landing a tag *constant* and actually routing a type's
//! `sign()`/`verify()` through it are two different commits, staged across
//! several tasks so each node crate's own tests move in lockstep with its
//! own migration. That means, mid-program, most `Signed*` types still sign
//! over plain `json::to_bytes`, exactly as before — so the shape guard below
//! is left doing real, unweakened work rather than being satisfied
//! vacuously.
//!
//! Statically proving "every `Signed*` type's `sign()`/`verify()` calls
//! `domain::preimage`" from *this* crate is not possible: `serialization` is
//! a dependency of every node crate, not the other way around, so it cannot
//! see their source at compile time, and a source-scanning integration test
//! only sees files under this crate's own directory tree by construction of
//! `CARGO_MANIFEST_DIR`. This test instead reads the whole workspace (as the
//! shape check below already did) and adds a second, complementary check:
//! [`CLIENT_SIGNED_TYPES`] is the checklist of every `Signed*` wrapper known
//! at F-01 time, paired with the `tag::` constant a reviewer gave it. A
//! `const _: &str = domain::tag::X;` reference for each entry makes a
//! renamed-or-removed constant a compile error, and
//! `every_signed_type_is_on_the_f01_checklist` fails the build if the
//! workspace scan finds a `Signed*` type that is not in the checklist —
//! catching a new type landing without ever being triaged for a tag. It does
//! not (and structurally cannot, from here) prove that a *listed* type has
//! actually finished moving its `sign`/`verify` over; that is what each
//! per-crate migration task's own round-trip tests are for.
//!
//! # What the shape check flags
//!
//! Any two payload types reached by a `Signed*` wrapper that share a field
//! shape, unless both already sign under distinct domain tags. It is
//! deliberately noisy in one direction: a same-shaped pair that is genuinely
//! fine still has to be acknowledged, by giving it a tag.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use openfiat_serialization::domain::tag;

/// Payload structs that legitimately share a shape *and* already sign under
/// distinct domain tags, so the confusion is closed. Anything here must have
/// a `tag::` constant in `domain.rs` and use it on both sign and verify.
const DOMAIN_SEPARATED: &[&str] = &[
    "ClaimVerify",
    "ClaimRevoke",
    "ProposalWithdraw",
    "ProposalActivate",
];

/// Every `Signed*` wrapper type known to exist in the workspace as of F-01,
/// paired with the `domain::tag` constant a reviewer assigned it. A new
/// `Signed*` type that is not added here (with a tag, or a documented reason
/// it needs none) fails [`every_signed_type_is_on_the_f01_checklist`] —
/// this is the "did anyone look at this" gate for domain separation.
///
/// The seven node-internal types tagged before F-01, plus the client-signed
/// types F-01 added, are all listed — this is meant to be the *complete*
/// set, not just the new ones, so the checklist stays a single source of
/// truth instead of splitting across two lists that can drift apart.
const CLIENT_SIGNED_TYPES: &[(&str, &str)] = &[
    ("SignedClaimVerify", tag::CLAIM_VERIFY),
    ("SignedClaimRevoke", tag::CLAIM_REVOKE),
    ("SignedProposalWithdraw", tag::PROPOSAL_WITHDRAW),
    ("SignedProposalActivate", tag::PROPOSAL_ACTIVATE),
    ("SignedMutualSettlementAgree", tag::MUTUAL_SETTLEMENT_AGREE),
    ("SignedPaymentMethodDefine", tag::PAYMENT_METHOD_DEFINE),
    ("SignedFeeSettlement", tag::FEE_SETTLEMENT),
    ("SignedAdvertisementCreate", tag::ADVERTISEMENT_CREATE),
    ("SignedAdvertisementStatusSet", tag::ADVERTISEMENT_STATUS_SET),
    ("SignedAdvertisementTermsUpdate", tag::ADVERTISEMENT_TERMS_UPDATE),
    ("SignedAdvertisementPriceUpdate", tag::ADVERTISEMENT_PRICE_UPDATE),
    ("SignedReservationRequest", tag::RESERVATION_REQUEST),
    ("SignedReservationCancel", tag::RESERVATION_CANCEL),
    ("SignedSettlementInitiate", tag::SETTLEMENT_INITIATE),
    ("SignedPaymentSubmitted", tag::PAYMENT_SUBMITTED),
    ("SignedPaymentReversed", tag::PAYMENT_REVERSED),
    ("SignedSettlementApproved", tag::SETTLEMENT_APPROVED),
    ("SignedSettlementRejected", tag::SETTLEMENT_REJECTED),
    ("SignedSettlementCancelled", tag::SETTLEMENT_CANCELLED),
    ("SignedRegistration", tag::REGISTRATION),
    ("SignedHealthUpdate", tag::HEALTH_UPDATE),
    ("SignedWithdrawal", tag::WITHDRAWAL),
    ("SignedSessionCreate", tag::SESSION_CREATE),
    ("SignedSessionRenew", tag::SESSION_RENEW),
    ("SignedSessionRevoke", tag::SESSION_REVOKE),
    ("SignedSessionMigrate", tag::SESSION_MIGRATE),
    ("SignedReviewPublish", tag::REVIEW_PUBLISH),
    ("SignedRiskPublish", tag::RISK_PUBLISH),
    ("SignedOraclePublish", tag::ORACLE_PUBLISH),
    ("SignedSnapshotAnnounce", tag::SNAPSHOT_ANNOUNCE),
    ("SignedDisputeOpen", tag::DISPUTE_OPEN),
    ("SignedArbitratorJoin", tag::ARBITRATOR_JOIN),
    ("SignedVoteCommit", tag::DISPUTE_VOTE_COMMIT),
    ("SignedVoteReveal", tag::DISPUTE_VOTE_REVEAL),
    ("SignedProposalCreate", tag::PROPOSAL_CREATE),
    ("SignedVoteCast", tag::VOTE_CAST),
    ("SignedClaimPublish", tag::CLAIM_PUBLISH),
    ("SignedAttachmentPublish", tag::ATTACHMENT_PUBLISH),
    ("SignedSubscriptionUpdate", tag::SUBSCRIPTION_UPDATE),
    ("SignedDeliveryReport", tag::DELIVERY_REPORT),
    ("SignedTradeChannelKeyGrant", tag::TRADE_CHANNEL_KEY_GRANT),
    ("SignedTradeChannelEntryPost", tag::TRADE_CHANNEL_ENTRY_POST),
    // Deliberately NOT here, and out of scope for F-01:
    // - `discovery::SignedAdvertisement` (peer-gossip identity, signs over
    //   `wire::to_bytes` rather than JSON — a different contract entirely).
    // - `wallet::SignedRequest<T>` (a generic, not-yet-wired future RPC-auth
    //   envelope; its `{payload, wallet, nonce, timestamp}` shape already
    //   includes a nonce, which is not a bare marketplace event payload).
    // A reviewer adding either to real traffic must re-evaluate whether it
    // belongs on this list.
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

/// Every identifier in `text` that starts with `Signed`, is a distinct word
/// (not a substring of a longer identifier), and is immediately followed —
/// skipping whitespace — by an opening brace. This matches both an
/// ordinary `pub struct` definition and this workspace's one
/// macro-generated case (`settlement_action!` in
/// `crates/settlement/src/events.rs`), whose struct body is still written
/// out literally as a macro argument, name-then-brace exactly like a plain
/// struct definition. It also matches a struct literal expression
/// constructing an already-known type, which just means that name gets
/// found more than once — harmless, since the result is deduplicated.
///
/// (Note for anyone editing this comment: avoid writing a `Signed`-prefixed
/// name directly followed by a brace here, or this very doc comment will
/// trip the scan when this file is itself included in the workspace walk.)
fn signed_type_names_followed_by_brace(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut names = Vec::new();
    for (index, _) in text.match_indices("Signed") {
        // Require a word boundary before the match, so `LastSigned` or
        // `unsigned` do not count as a name starting with `Signed`.
        if index > 0 {
            let prev = bytes[index - 1];
            if prev.is_ascii_alphanumeric() || prev == b'_' {
                continue;
            }
        }
        let rest = &text[index..];
        let ident_len = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        let name = &rest[..ident_len];
        // `Signed` alone (e.g. from `Signed*` in a doc comment, which is
        // not a real identifier at all) never names a type.
        if name == "Signed" {
            continue;
        }
        let after = rest[ident_len..].trim_start();
        if after.starts_with('{') {
            names.push(name.to_string());
        }
    }
    names
}

#[test]
fn every_signed_type_is_on_the_f01_checklist() {
    // Compile-time half of the guard: if any tag constant referenced by
    // `CLIENT_SIGNED_TYPES` were renamed or removed, this file would fail to
    // compile before the assertion below ever ran.
    let _: Vec<(&str, &str)> = CLIENT_SIGNED_TYPES.to_vec();

    let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/serialization has a parent");
    let sources = crate_sources(crates_dir);

    let checklist: std::collections::HashSet<&str> =
        CLIENT_SIGNED_TYPES.iter().map(|(name, _)| *name).collect();

    // Types this program has deliberately decided are out of scope for a
    // client-signed `/v1` tag — see the comment at the end of
    // `CLIENT_SIGNED_TYPES`. Anything else found on the wire but missing
    // from both lists is what this test exists to catch.
    let out_of_scope: std::collections::HashSet<&str> =
        ["SignedAdvertisement", "SignedRequest"].into_iter().collect();

    let mut found: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (_, text) in &sources {
        for name in signed_type_names_followed_by_brace(text) {
            found.insert(name);
        }
    }

    let mut untriaged: Vec<&String> = found
        .iter()
        .filter(|name| !checklist.contains(name.as_str()) && !out_of_scope.contains(name.as_str()))
        .collect();
    untriaged.sort();

    assert!(
        untriaged.is_empty(),
        "found `Signed*` type(s) not on the F-01 checklist and not marked out of scope: \
         {untriaged:?}. Either give it a domain tag in openfiat_serialization::domain::tag \
         and add it to CLIENT_SIGNED_TYPES in this file, or add it to `out_of_scope` here \
         with a comment explaining why it needs no tag."
    );
}
