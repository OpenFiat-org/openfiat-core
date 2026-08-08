# Design — Security hardening batch 1 (F-03, F-04, F-05)

Status: **Draft for review** · Date: 2026-08-08 · Scope: `openfiat-core`
(`programs/programs/{escrow,governance}`, `crates/identity`)

## Problem

Three of the open findings from the adversarial assessment are contained
enough to fix together in one review-gated branch, ahead of the external
audit that gates mainnet. The two large/cross-repo findings (F-01 coordinated
domain separation, F-02 anti-spam/DoS) get their own spec+plan cycles later.

Maintainer decisions (2026-08-08):
- **F-03** — ban list should **freeze new engagements only**.
- **F-05** — **rename `Verified` → `SelfAttested` now**, design a real
  verifier-authority `Verified` as a future addition (documented, not built).

## F-04 — minimum voting period (governance)

**Gap:** `programs/programs/governance/src/instructions/create_proposal.rs:102`
enforces only `require!(voting_period_secs > 0, ErrorCode::InvalidVoteLock)`.
A proposer can set a 1-second voting window and finalize a proposal before
anyone can vote against it.

**Fix:** add a floor.
- New constant in `programs/programs/governance/src/constants.rs`:
  `MIN_VOTING_PERIOD_SECS: i64 = 86_400` (24 hours). **[DEVNET VALUE]** — set
  to 24h so governance-cycle tests don't have to wait days. It carries a loud
  comment: **MUST be raised to `604_800` (7 days) before mainnet** — it is a
  compile-time constant, so the mainnet program build must bump it and this is
  a hard pre-mainnet gate (tracked in the mainnet launch register /
  deploy-mechanics lane). Recorded as a named protocol parameter, not an
  implementation detail.
- In `create_proposal.rs`, replace the `> 0` check with
  `require!(voting_period_secs >= MIN_VOTING_PERIOD_SECS, ErrorCode::VotingPeriodTooShort)`
  (append `VotingPeriodTooShort` to the governance `ErrorCode` enum — check the
  file's numbering convention; append, don't reorder). Keep any existing
  upper-bound check if present.

**Tests** (`programs/tests/governance.ts` or `governance-cycle.ts`): a proposal
with `voting_period_secs` just below the floor is rejected with
`VotingPeriodTooShort`; one at exactly the floor succeeds.

## F-03 — ban list freezes new engagements only

**Gap:** the ban check (`openfiat_programs_shared::wallet_is_banned` against a
`[BAN_SEED, wallet]` PDA under the governance program) is enforced on entry
paths (`deposit_liquidity`, `fund_rewards_vault`, `stake`, `create_proposal`,
`list_wallet`, `delist_wallet`) but a banned wallet can still **start new
trades** (`reserve_liquidity`, `create_trade_escrow`) and **open new disputes**
(`open_dispute_case`).

**Decision — freeze new engagements only:** block a banned wallet from
*starting* something new, but never strand an in-flight engagement — finishing
paths (`release_escrow`, `commit_dispute_vote`, `reveal_dispute_vote`,
`execute_dispute_outcome`, `claim_*`, `withdraw_liquidity`) stay ungated so an
existing counterparty/arbitrator is not frozen out mid-trade.

**Fix:** replicate the exact `deposit_liquidity` pattern into three
instructions. The pattern:
```rust
/// CHECK: ban gate by proof-of-non-existence — see deposit_liquidity.rs.
#[account(
    seeds = [openfiat_programs_shared::BAN_SEED, <signer>.key().as_ref()],
    bump,
    seeds::program = openfiat_programs_shared::GOVERNANCE_PROGRAM_ID,
)]
pub ban_record: UncheckedAccount<'info>,
```
plus, at the top of the handler:
```rust
require!(
    !openfiat_programs_shared::wallet_is_banned(&ctx.accounts.ban_record),
    ErrorCode::WalletBanned,
);
```

Target instructions and the signer whose key seeds `ban_record` (confirm the
actual signer field name while implementing — `reserve_liquidity` and
`create_trade_escrow` both name their signer `merchant`, `open_dispute_case`'s
opener must be read from the file):
- `programs/programs/escrow/src/instructions/reserve_liquidity.rs`
- `programs/programs/escrow/src/instructions/create_trade_escrow.rs`
- `programs/programs/escrow/src/instructions/open_dispute_case.rs`

Add a `WalletBanned` variant to escrow's `ErrorCode` if it doesn't already have
one (append per the file's numbering convention). `open_dispute_case` is in the
escrow program per the file listing.

**Tests** (`programs/tests/{escrow,ban-list}.ts`): for each of the three, a
banned signer is rejected with `WalletBanned`; an unbanned signer succeeds
(the ban PDA is empty). Assert a finishing path (e.g. `release_escrow` or
`reveal_dispute_vote`) is NOT gated — a wallet banned mid-trade can still
complete/settle — to lock in the "new engagements only" decision as a test.

## F-05 — self-asserted "Verified" claim

**Gap:** `crates/identity/src/record.rs:129-133` —
`VerificationStatus { Unverified, Verified }` on a `Claim` that is signed by the
claiming wallet itself. `store.rs` sets `Verified` from the claimant's own
signature (confirm the exact trigger while implementing), so a counterparty who
trusts a "Verified" badge is trusting the claimant's self-assertion.

**Decision — SelfAttested now, verifier-authority Verified later:**
- Rename the enum variant `Verified` → `SelfAttested` throughout
  `crates/identity` (`record.rs` definition + doc, `store.rs` set/compare sites
  at ~lines 90/123/241/600, `lib.rs` re-export, `tests/replication.rs`). The
  logic that sets it is unchanged — only the name becomes honest.
- The enum becomes `{ Unverified, SelfAttested }`. Add a doc comment on the enum
  describing the planned future `Verified` variant: it will require a signature
  from a **distinct verifier authority** (not the claimant), and is
  deliberately **not** added yet because there is no verifier-authority design
  or key on devnet. Do NOT add an unreachable `Verified` variant.
- **Out of scope / do not touch:** `crates/reputation`'s
  `MerchantTier::Verified` — a completely unrelated tier enum. F-05 is only
  about `identity::VerificationStatus`.

**Blast radius is `crates/identity` only** — confirmed no `openfiat-sdks` or
`openfiat-app` reference `identity::VerificationStatus`. If any generated
snapshot/JSON test fixture serializes the string `"Verified"` for a claim,
update it to `"SelfAttested"`.

**Tests** (`crates/identity` unit + `tests/replication.rs`): existing tests that
assert `VerificationStatus::Verified` become `SelfAttested`; add/keep a test
that a self-signed claim renders as `SelfAttested`, never a status implying
third-party verification.

## Verification

- `cargo test -p openfiat-identity` green (F-05).
- `anchor test` presale-unaffected; governance + escrow suites green (F-03, F-04)
  — run the relevant describes (governance, escrow, ban-list) via the scoped
  glob, full suite once before merge.
- `cargo build --workspace` / `cargo clippy` clean (F-05 rename ripples).
- Each finding independently reviewed; whole-branch review before merge.

## Out of scope (separate later cycles)

- F-01 — coordinated domain separation across node + Rust SDK + TS SDK + app
  with a version gate (large, cross-repo).
- F-02 — anti-spam/DoS architecture (stake/credit-gated origination, per-wallet
  claim caps + pruning, RPC auth/rate-limit, gossip/operator port split).
