# Design — F-01: coordinated domain separation for client-signed wire types

Status: **Draft for review** · Date: 2026-08-08 · Scope: `openfiat-core`
(node crates + tags + conformance vectors), `openfiat-sdks` (TS SDK; Rust SDK
re-pin only), `openfiat-app`.

## Problem

`crates/serialization/src/domain.rs` (shipped in `34d753f`) domain-separates
signing preimages as `preimage(tag, payload) = len(tag):u32be ‖ tag ‖ json(payload)`,
binding a payload's type into the bytes signed so a signature for one type
can't be replayed as another with the same field shape. Only the **7
node-internal** signed types are tagged. The **client-signed** wire types
(~25–30 `Signed*` types the SDKs and app sign, verified by the node) still sign
over plain `json::to_bytes(payload)` with no tag — the exploitable collision
class. F-01 closes this across all four signing surfaces in one coordinated
release.

Maintainer decisions (2026-08-08): **hard cutover / flag day** (no dual-accept
window — node rejects untagged after the change; tags carry `/v1`); **full
change, one program** (node + Rust SDK + TS SDK + app + conformance test,
landing together).

## The load-bearing insight (shrinks the work)

The Rust SDK does **not** re-implement signing — it calls the node crates'
constructors: `openfiat-sdks/rust/.../advertisements.rs:248` does
`SignedAdvertisementCreate::sign(create, keypair)`, importing
`openfiat_advertisements::events::SignedAdvertisementCreate`. Sign **and**
verify for each type live in one place — the node crate's `events.rs`. So:

- Changing each type's `sign()` + `verify()` in the node crate (both move to
  `domain::preimage` **together**) covers the node, the node's own tests, AND
  the Rust SDK in one edit. The Rust SDK just **re-pins** to the new core commit.
- Only the **TS SDK** and the **app** reconstruct the preimage independently
  (different codebases), so only they need a hand-written header helper.

## Invariants (must hold; the whole design rests on these)

1. **Sign and verify move together, per type.** For each type, `sign()` and
   `verify()` must both switch to `domain::preimage(TAG, payload)` in the same
   commit — a half-migrated type fails its own round-trip.
2. **The JSON body is unchanged.** Only the `len‖tag‖` header is prepended.
   Field order / serialization of the payload must not change, or the existing
   cross-language byte-match (Rust `json::to_bytes` vs TS `JSON.stringify`,
   which match today) breaks. Do NOT touch payload structs.
3. **One tag string per type, identical across all four surfaces.** The tag
   literal in `domain::tag` (Rust) and in the TS SDK / app tag tables must be
   byte-identical. Enforced by the conformance test (below), not by hope.
4. **Hard cutover.** The node verifies ONLY the tagged preimage after this
   change. There is no fallback to untagged. Safe because there is no live
   mainnet client fleet — devnet upgrades in lockstep.

## Scope: which types

Every currently-untagged `Signed*` type (verified via plain `json::to_bytes`).
By crate (implementers grep each `events.rs`/`store.rs` for `json::to_bytes`
used in a `sign`/`verify` and confirm against this list):
advertisements (Create, PriceUpdate, StatusSet, TermsUpdate), reservations
(Request, Cancel), settlement (Initiate + the payment/reversal/rejection
variants), registry (Registration, HealthUpdate, Withdrawal — FeeSettlement is
already tagged), sessions (Create, Renew, Migrate, Revoke), reviews
(ReviewPublish), risk (RiskPublish), oracles (OraclePublish), snapshot
(SnapshotAnnounce), disputes (DisputeOpen + vote commit/reveal/cast if
client-signed), governance (ProposalCreate + VoteCast if client-signed),
identity (ClaimPublish/AttachmentPublish if untagged), notifications
(DeliveryReport, SubscriptionUpdate), trade-channel (EntryPost, KeyGrant),
arbitrator join. The 7 already-tagged types stay as they are. Node-internal
untagged stragglers (e.g. SnapshotAnnounce, if node-signed) get tagged too —
harmless and completes the separation; only the client-signed subset needs
TS/app changes.

Tag scheme: `openfiat/<domain>/<Type>/v1`, matching the existing 7.

## Work breakdown (tasks land together; hard cutover)

### A. openfiat-core (the source of truth)
1. **Tags**: add one `/v1` tag per newly-covered type to `domain::tag`; update
   the `domain.rs` module doc (the "client-signed types deliberately absent"
   paragraph becomes "now included; the cross-repo contract is frozen by
   `tests/conformance_vectors`").
2. **Node crates**: for each type, switch `sign()` + `verify()` to
   `domain::preimage(tag::X, &payload)`. Group by crate (one task per crate or
   a few small crates together). Each crate's own tests must stay green
   (they sign+verify via the same constructors, so they move in lockstep).
3. **Structural guard**: update `crates/serialization/tests/signed_payload_shapes.rs`.
   Once every signed type is tagged, the "two untagged payloads share a shape"
   guard is satisfied vacuously; replace it with the stronger invariant it was
   standing in for — **every `Signed*` type's preimage is domain-tagged** (or,
   minimally, keep it and assert the tagged set now covers what were the
   collisions). Decide the exact shape while implementing; it must still fail
   the build if a new untagged signed type is added.
4. **Conformance vectors**: a Rust test/bin emits a checked-in JSON file of
   `{tag, payload_json, preimage_hex}` fixtures (one per client-signed type),
   the frozen cross-repo contract. A Rust test asserts `domain::preimage`
   reproduces each vector.

### B. openfiat-sdks
5. **TS SDK**: add `src/domain.ts` — `preimage(tag: string, payloadJson: string): Uint8Array`
   implementing `len:u32be ‖ utf8(tag) ‖ utf8(payloadJson)` — and a `tags.ts`
   table mirroring the Rust literals. Change every client sign site
   (~15–18: advertisements, providers/registry, reservations, oracles,
   settlement, notifications) from
   `encode(JSON.stringify(x))` → `preimage(tags.X, JSON.stringify(x))`. Add a
   test reading openfiat-core's conformance-vector file and asserting
   `preimage()` matches byte-for-byte. Re-pin the Rust SDK to the new core and
   confirm its live-node tests pass (auto-covered).
### C. openfiat-app
6. **App**: same header helper + tag table (`lib/`), applied to the app's
   direct sign sites (`lib/arbitration.ts` and any other `JSON.stringify` +
   `signMessage` site), with the same conformance-vector test.

### D. Document the breaking change (release notes) — REQUIRED
This is a **breaking wire-protocol change**: signatures produced by any
pre-F-01 signer (old SDK/app) no longer verify against a post-F-01 node, and
vice versa. It must be documented, not shipped silently:
- **openfiat-core** `CHANGELOG.md`: a `### Breaking` entry — "Client-signed
  wire events are now domain-separated (`domain::preimage`, `/v1` tags); a node
  on this version rejects signatures made with the pre-domain-separation
  preimage. All clients (SDKs, app) must upgrade in lockstep." Bump any
  protocol/wire version constant if one exists.
- **openfiat-sdks** TS SDK (`CHANGELOG.md` + `package.json`) and Rust SDK
  (`CHANGELOG.md` + `Cargo.toml`): breaking entry naming the domain-separation
  change and the minimum compatible node/core version. Pre-1.0, a breaking
  change bumps the **minor** (0.1.x → **0.2.0**) for both SDKs; the published
  release notes / npm+crates description must state the incompatibility with
  older nodes explicitly (consistent with the existing
  [[sdk_release_pipeline]] and the "subject to change" disclaimer).
- **openfiat-app**: a `CHANGELOG.md` / release note entry if the app keeps one,
  same substance.
The wording must let a consumer immediately understand: what broke, which
versions are mutually compatible, and that a mixed old-client/new-node (or
vice-versa) deployment will fail signature verification.

## Verification

- Per node crate: that crate's `cargo test` green (sign↔verify round-trips
  under the tag).
- `serialization` conformance-vector test green; structural guard still fails
  on an untagged addition.
- TS SDK + app conformance-vector tests green (byte-identical to Rust).
- Rust SDK + TS SDK **live-node** integration tests green against a node built
  from the new core — the real cross-surface proof (client signs tagged, node
  verifies tagged).
- Landing: node/core first (defines vectors + tags), then TS SDK + app + Rust
  SDK re-pin, all in the same release. Because it's a hard cutover, do NOT
  deploy the new node to devnet until the SDKs/app are ready, or in-flight
  old-client events will fail verification (acceptable but avoid the gap).

## Out of scope

- Nonce/expiry/replay protection and network separation (the domain doc notes
  these are separate concerns).
- Changing any payload struct's fields or serialization (would break the
  cross-language JSON body match).
