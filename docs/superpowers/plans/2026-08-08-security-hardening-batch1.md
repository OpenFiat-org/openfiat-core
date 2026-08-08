# Security hardening batch 1 (F-03, F-04, F-05) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close three contained adversarial-assessment findings: a governance minimum voting period (F-04), freezing new engagements for banned wallets (F-03), and honestly naming self-asserted identity claims (F-05).

**Architecture:** Two Anchor programs (`governance`, `escrow`) and one node crate (`crates/identity`) in `openfiat-core`. Each finding is independent; each ends with its own tests green.

**Tech Stack:** Rust 1.89 / Anchor 1.1.2 (programs), Rust workspace + `cargo test` (crates). Program tests are TypeScript (`ts-mocha`).

## Global Constraints

- **Devnet only.** `mainnet-beta` must never appear in `programs/`.
- **Error codes / enum variants are APPEND-ONLY** in the Anchor programs (Anchor numbers by declaration order; reordering breaks clients). Append new variants at the end; never remove or reorder existing ones.
- **F-04 floor is a DEVNET value:** `MIN_VOTING_PERIOD_SECS = 86_400` (24h) with a loud comment that it MUST become `604_800` (7 days) before mainnet (compile-time constant → mainnet rebuild). This is a tracked pre-mainnet gate.
- **F-05 blast radius is `crates/identity` ONLY.** Do NOT touch `crates/reputation`'s unrelated `MerchantTier::Verified`.
- **Commits:** conventional messages, no Claude attribution. Work on the current branch; do NOT push (controller merges at end).
- **Program test running:** the full `anchor test` suite is ~36 min. For per-task iteration the CONTROLLER runs a scoped subset by temporarily pointing the `Anchor.toml` `[scripts] test` glob at the relevant file(s), then reverting. Implementers do `anchor build` (compile check) + commit; the controller runs the tests. The `crates/identity` tests (F-05) are fast — the implementer runs those directly.

---

### Task 1: F-04 — minimum voting period floor (governance)

**Files:**
- Modify: `programs/programs/governance/src/constants.rs` (new constant)
- Modify: `programs/programs/governance/src/error.rs` (append `VotingPeriodTooShort`)
- Modify: `programs/programs/governance/src/instructions/create_proposal.rs:102`
- Test: `programs/tests/governance.ts` (or `governance-cycle.ts` — use whichever already exercises `create_proposal`)

**Interfaces:**
- Produces: `governance::constants::MIN_VOTING_PERIOD_SECS: i64 = 86_400`; `ErrorCode::VotingPeriodTooShort`; `create_proposal` rejects `voting_period_secs < MIN_VOTING_PERIOD_SECS`.

- [ ] **Step 1: Add the constant**

In `constants.rs`, add (matching the file's `#[constant]` style; if the `#[constant]` macro rejects `i64`, use a plain `pub const`):

```rust
/// Minimum on-chain voting window for a governance proposal (OFS-4000).
///
/// [DEVNET VALUE] 24 hours, so governance-cycle tests need not wait days.
/// MUST be raised to 604_800 (7 days) before mainnet — this is a
/// compile-time constant, so the mainnet program build has to bump it.
/// Tracked as a hard pre-mainnet gate (mainnet launch register).
#[constant]
pub const MIN_VOTING_PERIOD_SECS: i64 = 86_400;
```

- [ ] **Step 2: Append the error variant**

In `error.rs`, append at the very end of the enum (after `EmptyOffchainIdHash`):

```rust
    #[msg("voting_period_secs is below the minimum voting period (MIN_VOTING_PERIOD_SECS)")]
    VotingPeriodTooShort,
```

- [ ] **Step 3: Enforce the floor**

In `create_proposal.rs`, replace line 102's check:

```rust
    require!(
        voting_period_secs >= crate::constants::MIN_VOTING_PERIOD_SECS,
        ErrorCode::VotingPeriodTooShort
    );
```

(This subsumes the old `> 0` check. Leave the `InvalidVoteLock` variant in `error.rs` in place — append-only; it may be used elsewhere.)

- [ ] **Step 4: Write the tests**

In the governance test file that exercises `create_proposal`, add two cases (match the file's existing helper for creating a proposal; a sale/proposal fixture already exists there):
- a proposal with `votingPeriodSecs` = `MIN - 1` (e.g. `86_399`) is rejected with `VotingPeriodTooShort`;
- a proposal with `votingPeriodSecs` = `86_400` succeeds.

Adjust any existing `create_proposal` test that passed a `voting_period_secs` below `86_400` — bump it to `>= 86_400` so it still passes.

- [ ] **Step 5: Compile check + commit**

Run `anchor build` (from `programs/`), confirm clean. Do NOT run the full suite (controller runs it). Commit to the branch (no push):

```bash
git add programs/programs/governance/src programs/tests/*.ts
git commit -m "fix(governance): enforce MIN_VOTING_PERIOD_SECS floor on create_proposal (F-04)"
```

---

### Task 2: F-03 — freeze new engagements for banned wallets (escrow)

**Files:**
- Modify: `programs/programs/escrow/src/instructions/reserve_liquidity.rs`
- Modify: `programs/programs/escrow/src/instructions/create_trade_escrow.rs`
- Modify: `programs/programs/escrow/src/instructions/open_dispute_case.rs`
- Test: `programs/tests/escrow.ts` and/or `programs/tests/ban-list.ts`

**Interfaces:**
- Consumes: `openfiat_programs_shared::{BAN_SEED, GOVERNANCE_PROGRAM_ID, wallet_is_banned}`; `escrow::ErrorCode::WalletBanned` (already exists, `error.rs:66`).
- Produces: each of the three instructions rejects a banned signer with `WalletBanned`; finishing paths remain ungated.

**The pattern (copied verbatim from `deposit_liquidity.rs:16-26,55-57`).** For each instruction, add this account to its `#[derive(Accounts)]` struct, with `<signer>` = that instruction's signing party (see per-instruction notes), and the `require!` as the FIRST statement of the handler.

Account (the `/// CHECK` doc must be present — it's a deliberate proof-of-non-existence gate):
```rust
    /// CHECK: OFS-7100 §12 ban gate, enforced by proof of non-existence —
    /// banned iff this canonical PDA is occupied; unchecked/uninitialized on
    /// purpose. seeds/seeds::program force the one canonical ban address for
    /// `<signer>`, so a banned caller cannot substitute an empty account.
    #[account(
        seeds = [openfiat_programs_shared::BAN_SEED, <signer>.key().as_ref()],
        bump,
        seeds::program = openfiat_programs_shared::GOVERNANCE_PROGRAM_ID,
    )]
    pub ban_record: UncheckedAccount<'info>,
```
Handler guard (first line):
```rust
    require!(
        !openfiat_programs_shared::wallet_is_banned(&ctx.accounts.ban_record),
        ErrorCode::WalletBanned,
    );
```

Per-instruction signer:
- `reserve_liquidity.rs` — signer is `merchant`. (On-chain only the merchant signs; the buyer's side is decided off-chain by the reservations crate — out of scope here.)
- `create_trade_escrow.rs` — signer is `merchant` (the seller).
- `open_dispute_case.rs` — signer is `signer` (constrained to the trade's buyer or seller).

- [ ] **Step 1: Add the ban gate to `reserve_liquidity.rs`**

Add the `ban_record` account (with `<signer>` = `merchant`) to `ReserveLiquidity` and the `require!` as the first line of `handle_reserve_liquidity`.

- [ ] **Step 2: Add the ban gate to `create_trade_escrow.rs`**

Same, with `<signer>` = `merchant`, in `CreateTradeEscrow` / its handler.

- [ ] **Step 3: Add the ban gate to `open_dispute_case.rs`**

Same, with `<signer>` = `signer`, in the open-dispute accounts struct / handler.

- [ ] **Step 4: Tests**

In `programs/tests/ban-list.ts` (preferred — it already has a `banWallet` helper) or `escrow.ts`, add cases that reuse the existing escrow/vault fixtures:
- a **banned** merchant calling `reserve_liquidity` is rejected with `WalletBanned`; an unbanned merchant succeeds.
- a **banned** merchant calling `create_trade_escrow` is rejected with `WalletBanned`.
- a **banned** trade party calling `open_dispute_case` is rejected with `WalletBanned`.
- **Locks in the decision:** a merchant banned AFTER a trade escrow exists can still complete a finishing path — assert `release_escrow` (or `reveal_dispute_vote`) is NOT gated and succeeds for a banned wallet. (Use the existing helper that bans a wallet; ban the relevant party after the escrow is created, then run the finishing instruction.)

- [ ] **Step 5: Compile check + commit**

`anchor build` clean. Commit (no push):

```bash
git add programs/programs/escrow/src programs/tests/*.ts
git commit -m "fix(escrow): freeze new engagements for banned wallets — reserve/create-escrow/open-dispute (F-03)"
```

---

### Task 3: F-05 — rename self-asserted `Verified` → `SelfAttested` (identity)

**Files:**
- Modify: `crates/identity/src/record.rs` (enum def + doc, ~line 129-133, and the field doc at line 7)
- Modify: `crates/identity/src/store.rs` (~lines 90, 123, 241, 600)
- Modify: `crates/identity/src/lib.rs` (re-export line 18 — `VerificationStatus` name stays; only the variant renames, so the re-export is unchanged unless a variant is named there)
- Modify: `crates/identity/tests/replication.rs` (~line 117)
- Test: same files (`cargo test -p openfiat-identity`)

**Interfaces:**
- Produces: `VerificationStatus { Unverified, SelfAttested }` (the `Verified` variant is renamed, not removed). The setter logic is unchanged.

- [ ] **Step 1: Rename the variant + document the future `Verified`**

In `record.rs`, change the enum and add the design note:

```rust
/// A claim's verification status.
///
/// `SelfAttested` means the claiming wallet signed the claim itself — it is
/// NOT third-party verification, and a consumer must not treat it as such.
/// It is named `SelfAttested` (not `Verified`) precisely so a counterparty
/// cannot mistake a self-signed claim for an externally attested one.
///
/// A future `Verified` variant will require a signature from a distinct
/// verifier authority (not the claimant). It is deliberately NOT added yet:
/// there is no verifier-authority design or key on devnet, and an
/// unreachable `Verified` would reintroduce exactly the ambiguity this
/// rename removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum VerificationStatus {
    Unverified,
    SelfAttested,
}
```
Also fix the module doc reference at `record.rs:7` ("whether a contact claim is actually `Verified`") to say `SelfAttested`.

- [ ] **Step 2: Update all set/compare sites**

In `store.rs`, replace every `VerificationStatus::Verified` with `VerificationStatus::SelfAttested` (the setter at ~line 123 that marks a self-signed claim, and the comparisons at ~90/241/600). The logic is unchanged — only the name.

- [ ] **Step 3: Update the test**

In `tests/replication.rs:117`, replace `VerificationStatus::Verified` with `VerificationStatus::SelfAttested`. Grep the whole `crates/identity` tree for any remaining `::Verified` and any JSON/snapshot fixture containing the string `"Verified"` for a claim's status; update those to `SelfAttested`.

- [ ] **Step 4: Run the tests + wider build**

```bash
cargo test -p openfiat-identity
cargo build --workspace
cargo clippy -p openfiat-identity
```
Expected: identity tests pass; workspace builds (catches any other crate that named the variant — none expected, but confirm). If another crate references the old variant, that's in scope to fix (it's the same rename); if it's `reputation::MerchantTier::Verified`, do NOT touch it.

- [ ] **Step 5: Commit**

```bash
git add crates/identity
git commit -m "refactor(identity): rename self-asserted VerificationStatus::Verified to SelfAttested (F-05)"
```

---

## Self-Review

**Spec coverage** (against `2026-08-08-security-hardening-batch1-design.md`):
- F-04 floor + error + test → Task 1. ✓ (devnet value 86_400 with the mainnet-bump note.)
- F-03 ban gate on the three new-engagement instructions + finishing-path-stays-ungated test → Task 2. ✓ (signer = merchant/merchant/signer; reuses existing `WalletBanned`.)
- F-05 rename + future-`Verified` doc, `crates/identity` only, reputation untouched → Task 3. ✓

**Placeholder scan:** none — every step has concrete code. The one runtime-confirmed detail (whether `#[constant]` accepts `i64`) has an explicit fallback (plain `pub const`).

**Type consistency:** `MIN_VOTING_PERIOD_SECS: i64` matches `voting_period_secs: i64`. `VerificationStatus` type name is unchanged; only the `Verified`→`SelfAttested` variant renames, applied consistently across record/store/lib/tests. `WalletBanned` is the existing escrow variant, not a new one.
