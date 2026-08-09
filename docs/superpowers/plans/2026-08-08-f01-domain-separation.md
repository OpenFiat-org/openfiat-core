# F-01 Coordinated Domain Separation — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Domain-separate every client-signed wire event (`domain::preimage`, `/v1` tags) across node + Rust SDK + TS SDK + app, in one coordinated hard-cutover release, with a cross-language conformance test and documented breaking-change release notes.

**Architecture:** The node crates own each type's `sign()`+`verify()` (the Rust SDK reuses them). Only the TS SDK and app reimplement the preimage header. A payload-agnostic conformance-vector file (`{tag, payload_json, preimage_hex}`) freezes the header layout across languages.

**Tech Stack:** Rust workspace (`cargo test`), TS SDK (`vitest`/node test), Next app (`vitest`).

## Global Constraints
- **Hard cutover.** Node verifies ONLY the tagged preimage; no untagged fallback. Sign+verify for a type MUST change together (same commit) or its round-trip breaks.
- **Do NOT change any payload struct's fields or serialization** — only prepend the `len:u32be ‖ tag ‖ ` header. The existing cross-language JSON-body match must be preserved.
- **One tag literal per type, byte-identical** in Rust `domain::tag`, TS `tags.ts`, and app tag table. Scheme: `openfiat/<domain>/<Type>/v1`.
- **Breaking change → release notes** in every repo touched (core + both SDKs + app), pre-1.0 SDKs bump to **0.2.0**.
- No Claude attribution in commits. This plan spans THREE git repos — each repo is its own review-gated branch (`sdd/f01-domain-separation`), landing in the coordinated order below. Commit; do NOT push (controller merges each).
- `mainnet-beta` must never appear in `programs/` (unrelated here, but holds).

## Repo order (hard cutover — core defines the contract first)
openfiat-core (Tasks 1–5) → openfiat-sdks (Tasks 6–7) → openfiat-app (Task 8) → docs/versions (Task 9). Core's conformance-vector file is **vendored** (copied) into the SDK and app repos (separate gits) and must match byte-for-byte.

---

### Task 1: Foundation — tags, conformance vectors, structural guard (openfiat-core)

**Files:**
- Modify: `crates/serialization/src/domain.rs` (tags + module doc)
- Create: `crates/serialization/tests/vectors/client_signed_v1.json` (the frozen contract)
- Create/Modify: `crates/serialization/tests/conformance_vectors.rs`
- Modify: `crates/serialization/tests/signed_payload_shapes.rs`

**Interfaces:**
- Produces: `openfiat_serialization::domain::tag::*` — one `pub const` per client-signed type (`/v1`). The vector file + a test proving `preimage(tag, bytes) == hex` for each row.

- [ ] **Step 1: Add tags.** In `domain.rs`'s `tag` module, add one `pub const` per client-signed type (names/scheme below), keeping the existing 7. Use the crate/type from the spec's scope list; confirm each against the actual `Signed*` types while implementing (grep `pub struct Signed` per crate). Update the module doc: the "client-signed types deliberately absent" paragraph now says they are included and the cross-repo contract is frozen by `tests/vectors/client_signed_v1.json`.

- [ ] **Step 2: Write the conformance vectors (payload-agnostic).** Create `tests/vectors/client_signed_v1.json`: an array of `{ "tag": "<tag string>", "payload_json": "<a representative JSON string>", "preimage_hex": "<hex>" }`. Include at least one row per tag. `payload_json` is treated as OPAQUE bytes — the vectors prove the HEADER (`len ‖ tag ‖ body`) is byte-identical across languages, independent of any struct. Compute `preimage_hex` with the real `preimage()`.

- [ ] **Step 3: Rust conformance test.** In `tests/conformance_vectors.rs`, read the JSON file and assert, for each row, `hex(domain::preimage(row.tag, row.payload_json.as_bytes())) == row.preimage_hex`. (Add a tiny `preimage` overload or reuse the existing one treating the payload as `&[u8]` via a `serde_bytes`-free path — the existing `preimage<T: Serialize>` will double-encode a String; instead expose/pupose a `preimage_raw(tag, body: &[u8])` that the `Serialize` version calls, and test `preimage_raw`.) Run: `cargo test -p openfiat-serialization`.

- [ ] **Step 4: Structural guard.** Update `signed_payload_shapes.rs`: its job was to fail the build if two UNTAGGED signed payloads shared a field shape. After this program every signed type is tagged, so change it to the stronger invariant — fail if any `Signed*` type is added whose sign/verify does NOT route through `domain::preimage` (or, if that's not statically detectable, keep the shape guard AND add a checklist test enumerating every tagged type so a new untagged one is caught in review). Implementer picks the enforceable form and documents why.

- [ ] **Step 5: Commit.** `git add crates/serialization && git commit -m "feat(serialization): add /v1 domain tags + conformance vectors for client-signed types (F-01)"` (no push).

---

### Task 2–5: Node crate sign+verify migration (openfiat-core)

Split the ~11 crates into balanced tasks (reviewer gate per task). Suggested grouping — one task each:
- **Task 2:** `advertisements` (4 types), `reservations` (2).
- **Task 3:** `registry` (Registration/HealthUpdate/Withdrawal — NOT FeeSettlement, already tagged), `settlement` (Initiate + variants).
- **Task 4:** `sessions` (4), `notifications` (DeliveryReport/SubscriptionUpdate).
- **Task 5:** `reviews`, `risk`, `oracles`, `snapshot`, plus any untagged `disputes`/`governance`/`identity`/`trade-channel` client types found (grep each for a `Signed*` whose `sign`/`verify` still uses `json::to_bytes`).

**Per-type pattern (identical everywhere):**
```rust
// in <crate>/src/events.rs (or store.rs), for each Signed<X>:
// sign():
-   let bytes = openfiat_serialization::json::to_bytes(&x).expect(...);
+   let bytes = openfiat_serialization::domain::preimage(
+       openfiat_serialization::domain::tag::<X>, &x).expect(...);
// verify(): the identical substitution on the same payload.
```
`sign()` and `verify()` for a type MUST both change in the same commit.

**Files (per task):** the crate's `src/events.rs`/`src/store.rs`; its tests.
**Interfaces:** consumes `domain::tag::*` from Task 1.

- [ ] For each type in the task's crates: apply the pattern to BOTH `sign()` and `verify()`.
- [ ] Run that crate's tests: `cargo test -p openfiat-<crate>` — the sign↔verify round-trip tests must stay green (they exercise both sides, so they prove the tag is applied consistently).
- [ ] Grep the crate to confirm no `json::to_bytes` remains in a sign/verify path (only non-signing serialization should remain).
- [ ] Commit: `git add crates/<crates> && git commit -m "feat(<crate>): domain-separate client-signed events (F-01)"` (no push).

After Task 5: run `cargo test --workspace` (controller) — every crate green, `serialization` conformance + guard green.

---

### Task 6: TS SDK domain header + sign sites + conformance (openfiat-sdks)

**Files:**
- Create: `typescript/src/domain.ts`, `typescript/src/tags.ts`
- Modify: the ~15–18 sign sites (`src/methods/{advertisements,providers,reservations,oracles,settlement,notifications}.ts`)
- Create: `typescript/tests/conformance_vectors.test.ts`
- Copy: `typescript/tests/vectors/client_signed_v1.json` (VENDORED from openfiat-core Task 1 — must be byte-identical)

**Interfaces:** consumes the tag literals (must match Rust) and the vector file.

- [ ] **Step 1:** `domain.ts`: `export function preimage(tag: string, body: Uint8Array): Uint8Array` = `len:u32be ‖ utf8(tag) ‖ body`. `tags.ts`: one `export const` per type mirroring the Rust literals EXACTLY.
- [ ] **Step 2:** at each sign site, `encode(JSON.stringify(x))` → `preimage(tags.X, encode(JSON.stringify(x)))`. Map each payload var (create/set/update/request/cancel/registration/withdrawal/publish/initiate/report/update…) to its tag. Challenge-based sites (wallet/earnings) already domain-separate — leave them.
- [ ] **Step 3:** vendor core's `client_signed_v1.json` into `typescript/tests/vectors/` and add `conformance_vectors.test.ts` asserting `bytesToHex(preimage(row.tag, utf8(row.payload_json))) === row.preimage_hex` for every row.
- [ ] **Step 4:** run the TS test suite (`pnpm test` in `typescript/`) — conformance + existing unit tests green.
- [ ] **Step 5:** commit (no push): `git commit -m "feat(ts-sdk): domain-separate signed wire events; conformance vectors (F-01)"`.

---

### Task 7: Rust SDK re-pin + verify (openfiat-sdks)

The Rust SDK is auto-covered (it calls the node crates' `sign()`). It only needs its `openfiat-core` git pin bumped to the branch's core commit.
- [ ] Bump the `rev`/pin in `rust/Cargo.toml` (and any `[patch]`) to the core commit carrying Tasks 1–5.
- [ ] `cargo build -p openfiat-sdk` clean; run the Rust SDK's OFFLINE tests (`cargo test -p openfiat-sdk` excluding live-node) — sign↔verify round-trips green. (Live-node cross-surface proof happens in the controller's final integration run.)
- [ ] Commit (no push): `git commit -m "chore(rust-sdk): re-pin core to domain-separated commit (F-01)"`.

---

### Task 8: App domain header + sign sites + conformance (openfiat-app)

**Files:**
- Create: `lib/domain.ts`, `lib/signing-tags.ts` (or reuse `@openfiat/sdk`'s exports if the app depends on it — prefer importing from the published SDK to avoid a third copy; if the app doesn't take that dep, add a local helper)
- Modify: the app's direct sign sites (`lib/arbitration.ts` + any other `JSON.stringify` + `signMessage`)
- Create: `tests/conformance_vectors.test.ts` + vendored `tests/vectors/client_signed_v1.json`

- [ ] Prefer importing `preimage`/`tags` from `@openfiat/sdk` (Task 6) if the app already depends on it; else add a local `lib/domain.ts` mirror. Confirm which by reading the app's `package.json`.
- [ ] Apply the header at each direct sign site with the correct tag (match the type — arbitration votes, etc.).
- [ ] Vendor the vector file + conformance test; run `pnpm test` (or the app's vitest) — green.
- [ ] Commit (no push): `git commit -m "feat(app): domain-separate directly-signed wire events (F-01)"`.

---

### Task 9: Breaking-change release notes + SDK version bumps (all repos)

- [ ] **openfiat-core** `CHANGELOG.md`: `### Breaking` entry — client-signed wire events are now domain-separated (`/v1` tags); a node on this version rejects pre-domain-separation signatures; all clients must upgrade in lockstep. Bump a wire/protocol version constant if one exists.
- [ ] **openfiat-sdks** TS SDK `package.json` 0.1.x → **0.2.0** + `CHANGELOG.md` breaking entry (names the change + minimum compatible node); Rust SDK `Cargo.toml` → **0.2.0** + `CHANGELOG.md` breaking entry.
- [ ] **openfiat-app** CHANGELOG/release note if it keeps one.
- [ ] Commit in each repo (no push).

---

## Self-Review
**Spec coverage:** tags (T1), node+RustSDK sign/verify (T2–5,7), conformance vectors + guard (T1, T6, T8), TS SDK (T6), app (T8), breaking-change docs + version bumps (T9). ✓
**Placeholder scan:** the one non-mechanical decision (structural-guard form in T1S4) is explicitly flagged for the implementer to choose + document. The per-type tag list is grep-confirmed against real `Signed*` types during T1/T2–5 rather than hardcoded here (the source-of-truth is the code).
**Type consistency:** `preimage_raw(tag, &[u8])` (Rust) ↔ `preimage(tag, Uint8Array)` (TS) ↔ app — all `len:u32be ‖ tag ‖ body`; tag literals identical across `domain::tag`, `tags.ts`, app; vector file byte-identical (vendored copies).
**Cross-repo:** landing order enforces hard cutover; do not deploy the new node to devnet until SDK+app are ready (avoid the verification gap) — a deploy/ops note, not a code task.
