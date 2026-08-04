# OpenFiat on-chain programs

Anchor workspace for the OpenFiat protocol's Solana programs. This is a
**separate Cargo/Anchor workspace** from `../crates/` (the off-chain node) —
Anchor pins its own `solana-sdk`/dependency versions, so the two are kept
isolated rather than sharing one workspace.

> **DEVNET ONLY.** Every program ID, keypair, and cluster reference in this
> directory is a devnet artifact. Mainnet deployment is a separate future
> phase gated on (1) an external security audit of every program here and
> (2) final, non-draft sign-off on
> [OFS-4100 — Tokenomics Specification](../../openfiat-specs/Whitepaper/Specifications/OFS-4100%20-%20OpenFiat%20Tokenomics%20Specification%20(OTS).md).
> CI (`programs-ci.yml`) fails the build if `mainnet-beta` appears anywhere
> in this directory, as a guardrail.

See [OFS-4200 — On-Chain Program Architecture](../../openfiat-specs/Whitepaper/Specifications/OFS-4200%20-%20OpenFiat%20Onchain%20Program%20Architecture%20(OPA).md)
for the full design (PDA seed schemes, account layouts, instruction sets)
behind each program below.

## Programs

| Program | Status | Purpose |
|---|---|---|
| `presale` | Done (Phase 3) — full sale lifecycle, deployed to devnet | OPEN token presale (OFS-4200 §3) |
| `escrow` | Done (Phase 4 + 4b) — full vault lifecycle + dispute-to-chain bridge | Liquidity Vault + Trade Escrow Vault settlement |
| `staking` | Done (Phase 5a) — stake/unbond/withdraw/slash/rewards | Per-role OPEN staking, unbonding, slashing |
| `governance` | Done (Phase 5b) — proposals/voting/tally/deposit settlement | Proposals, voting, parameter updates, treasury spend |

## Prerequisites

- Rust (version pinned by `rust-toolchain.toml`, currently 1.89.0 — installed automatically by `rustup` on first build)
- [Solana CLI](https://docs.anza.xyz/cli/install) — devnet-configured (see below)
- Anchor CLI 1.1.2 (`cargo install anchor-cli --version 1.1.2 --locked`)
- Node.js 20+ (for the Mocha test suite)

## Local setup

```bash
# One-time: point the Solana CLI at devnet, never mainnet-beta.
solana config set --url https://api.devnet.solana.com

# One-time: generate a local dev keypair if you don't have one.
solana-keygen new -o ~/.config/solana/id.json

npm install
```

## Build and test

```bash
anchor build

# This Anchor CLI version defaults to the `surfpool` validator, which isn't
# installed here — `legacy` uses the standard solana-test-validator instead.
anchor test --validator legacy
```

`anchor test` starts an ephemeral local validator automatically (since
`Anchor.toml`'s `[provider] cluster` is `localnet`, not `devnet`) — this is
deliberate, so CI and local iteration stay fast and deterministic.

### The shared fixtures, and the two mints

Every spec under `tests/` runs in one mocha process against one ledger, so
the three protocol singletons — `FeeConfig`, `StakingConfig`,
`GovernanceConfig` — are initialized exactly once, by
`tests/shared-fixtures.ts`. Anything a spec needs from them it must get
from there.

The fixture creates **two** mints, and which one each singleton is
denominated in is a protocol fact rather than a fixture convenience:

| | mint | why |
|---|---|---|
| trade escrows, liquidity vaults, settlement fees | settlement | the stablecoin a trade is priced in; on `FeeConfig`'s allowlist |
| stake, arbitration pool, dispute filing fee, vote weight, proposal deposits | OPEN | OFS-4100 §1, §4, §6 |

They must not be collapsed into one — `execute_dispute_outcome` rejects a
merchant whose OPEN vault and settlement vault are the same account — and
the OPEN ones must not be split apart: `recover_stake_shortfall` transfers
straight from the stake vault into a merchant's OPEN liquidity vault, so a
`StakingConfig` denominated in anything but the arbitration pool's mint
describes a cluster the SPL token program will not run. Both directions
have been got wrong here before, which is why the fixture spells it out.

A spec that needs a *different* value for a mutable field — a non-zero
`dispute_filing_fee`, a longer vote lock, a narrower settlement allowlist —
writes it in `before` and hands it back in `after`. `stake-recovery.ts`,
`settlement-mints.ts` and `governance.ts` all do this. A spec that would
need a different value for an **init-once** field cannot, and would have to
run on a ledger of its own; there is no such spec today.

Actual devnet deployment is a separate, explicit step:

```bash
# Requires devnet SOL in the deploying wallet — request via
# `solana airdrop 2` or a devnet faucet if rate-limited.
solana program deploy \
  --url devnet \
  --program-id <PROGRAM_ID from devnet-addresses.json> \
  target/deploy/<program>.so
```

> **Upgrade in place, with an explicit `--program-id`. Never `anchor deploy`
> against devnet, and never `anchor keys sync`.**
>
> The program keypairs under `target/deploy/` were destroyed by a `cargo
> clean` and the files there now are freshly generated ones whose addresses
> do **not** match the deployed programs. `anchor deploy` deploys to the
> address of the local keypair file, so running it would publish a second
> copy of each program at a brand-new address, leaving the live one — with
> every config singleton, vault and staked token behind it — orphaned.
> `anchor keys sync` would do the same damage more quietly, by rewriting
> `declare_id!` to match the wrong keys. `anchor build` prints a program-ID
> mismatch warning for the same reason; it is expected, and building still
> produces correct binaries because `declare_id!` is the source of truth.
>
> An upgrade does not need the program keypair. `--program-id` accepts a
> plain address for an existing program, and authority comes from the
> upgrade authority (`devnet_programs.upgradeAuthority`), which is a
> separate key and must stay separate from the token mint authority.

After a real devnet deploy, record the resulting program ID in
`devnet-addresses.json` (created once the first program — `presale` — is
actually deployed; see Phase 3's exit criteria) — that file is the canonical
source every later phase (core wiring, SDKs, the app) reads from.

Verify every upgrade against the binary that was tested, rather than
trusting the deploy's success:

```bash
solana program dump <PROGRAM_ID> /tmp/onchain.so --url devnet
# The dump is the binary followed by zero padding to the account's
# capacity. Assert the padding is all zero BEFORE trimming it — trimming
# first and hashing the head matches even if the tail holds other bytes —
# then compare sha256 of the head against target/deploy/<program>.so.
```

## OPEN token genesis

`scripts/genesis.ts` creates the fixed-supply Token-2022 mint (OFS-4100 §1),
mints the full 1,000,000,000 OPEN supply once, distributes it across the 7
allocation buckets (OFS-4100 §2), then permanently revokes both mint and
freeze authority. `scripts/verify-genesis.ts` reads back on-chain state and
asserts every invariant (supply, decimals, revoked authorities, all 7 bucket
accounts distinct, balances sum to total).

The Community Presale bucket is owned by the `presale_vault` PDA (so the
presale program can release it in Phase 3 without a human keypair ever
custodying it). The other 6 buckets are each owned by a dedicated placeholder
keypair generated on first run and persisted under `scripts/.bucket-keys/`
(gitignored — never commit secret keys, even for devnet placeholders). Real
production custody (a multisig, and/or the vesting program for time-locked
buckets) is out of scope until the phases that actually spend from these
buckets are built.

```bash
# Local validator (fast, deterministic — this is what CI runs):
npx ts-node scripts/genesis.ts --cluster localnet
npx ts-node scripts/verify-genesis.ts --cluster localnet

# Real devnet (requires devnet SOL in the deploying wallet — see the
# `anchor deploy` note above re: airdrop rate-limiting):
npx ts-node scripts/genesis.ts --cluster devnet
npx ts-node scripts/verify-genesis.ts --cluster devnet
```

Results are recorded per-cluster in `devnet-addresses.json` (mint address,
admin pubkey, and each bucket's owner + token account address) — the
canonical source every later phase reads from. **As of this writing, the
real-devnet run is still pending**: the devnet SOL faucet has been
rate-limited in this environment on every attempt so far. Genesis has been
fully validated against a local `solana-test-validator` instead (all
`verify-genesis.ts` assertions pass); re-run the devnet commands above once
faucet access is available, or fund the deploying wallet manually via
https://faucet.solana.com.

## Arbitration parameters: the order governance switches them on in

Two arbitrator-eligibility parameters live on `escrow::FeeConfig` and both
ship at **zero — disabled**: the stake age gate
(`min_arbitrator_stake_age_secs`) and the opening sortition threshold
(`arbitrator_sortition_bps`). Zero is the only correct starting value for
each; nobody on a chain younger than a 30-day requirement can satisfy it, and
a draw needs a pool to draw from.

Governance turns them on through `update_fee_config`. **The order is
load-bearing and the reverse order is actively harmful.** This is
[OFS-4100 Annex A](../../openfiat-specs/Whitepaper/Analysis/OFS-4100%20Annex%20A%20-%20Arbitration%20Parameters%20Reviewed%20Together.md)
option C, and it is recorded here and beside the constants in
`escrow/src/constants.rs` because it was previously written down nowhere and
would otherwise be left to whoever drafts the governance proposal.

### 1. Stake age first

`min_arbitrator_stake_age_secs` → `RECOMMENDED_MIN_ARBITRATOR_STAKE_AGE_SECS`
(30 days, OFS-4100 §4).

It is the only arbitrator parameter that costs an attacker **time** rather
than capital, and time is what wallet manufacture cannot buy. The attack it
defends against is fifteen wallets at the 500 OPEN arbitrator minimum — about
7,500 OPEN, available in an afternoon — taking seats on a case and going
silent until it exhausts its rounds and lands on the terminal even split,
which is a guaranteed half of an escrow the attacker was going to lose
entirely. Capital does not deter that: the squatter never reveals outside
consensus, so the stake is locked, never slashed, and comes back.

Enable it **30 days after the first real arbitrators stake, not 30 days after
genesis.** Every live stake account's age clock starts at its own
`staking::migrate_stake_account`, so the chain's calendar age says nothing
about the age the pool can actually present. Switching it on too early locks
out the entire arbitrator pool, honest arbitrators included.

### 2. Sortition second, and only above a pool of ~17

`arbitrator_sortition_bps` → `RECOMMENDED_ARBITRATOR_SORTITION_BPS`
(100 bps, OFS-4100 §4.1).

The draw removes an attacker's ability to *choose* which seats to take. It
does nothing about an attacker *having enough wallets*, and on a small
network the second is what decides cases.

Worse, enabling it early makes things actively worse rather than merely no
better. Sortition admits a fraction of the eligible pool per round, so turning
it on **shrinks** the number of wallets that can take a seat in any given
round — at exactly the moment the barring rule is consuming the pool from the
other end. A case is only decidable at all on a pool of
`MIN_ARBITRATORS + MAX_BARRED_ARBITRATORS` = **17** (3 counted reveals in the
final round, plus the 14 wallets three rounds can retire for staying silent).
A tighter draw brings that structural-no-quorum point closer, and the even
split it produces is the thing the griefing party was buying.

So: enable the draw only once the eligible pool is **comfortably** above 17 —
comfortably, because 17 assumes every eligible wallet takes its seat and
reveals on time, which is not how a real round goes.

### Publishing the pool size

`publish_arbitrator_pool_size` (admin-gated) writes governance's count of
eligible arbitrators to the singleton `ArbitrationPolicy` account. It is what
the precondition above is checked against, and it also lets a case stop
instead of opening a round the pool cannot staff — recording
`TerminalSplitReason::PoolExhausted` rather than bouncing through its round
budget and splitting with no record of why (Annex A option A).

It is an **attestation, not a measurement**: a Solana program cannot
enumerate stake accounts, so nothing on chain can count the pool for itself.
Consequences, and they matter:

- **Zero means unpublished**, and disables the floor entirely. That is the
  shipped state on every cluster, and the correct state for any deployment
  that cannot keep the figure current. A stale-*low* figure would end live
  cases on the split sooner than the round budget would have — same payout,
  but sooner is what the attacker wanted.
- The floor never lets a published figure fall below the participation a case
  has actually seen, so a number the case has already outgrown cannot end it.
- `execute_dispute_outcome` reads the account as an optional trailing
  (`remainingAccounts`) entry, so a cluster that has never created it resolves
  disputes exactly as before.

The exact, self-maintaining source would be a counter on
`staking::StakingConfig` moved as arbitrator stakes cross the minimum. That
belongs to `openfiat-staking` and to a `StakingConfig` layout migration; the
escrow-side field is shaped so it can be replaced by one without the floor
changing.

## Linting and license compliance

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo deny check   # uses this directory's own deny.toml
```

Those three are the gates, and `programs-ci.yml` runs all three.

### There is deliberately no formatter for the TypeScript here

`package.json` used to carry `anchor init`'s stock
`prettier … --check` as `npm run lint`, with no `.prettierrc` beside it.
It failed on 29 of the 32 files under `tests/` and `scripts/` and had
done for as long as anyone had run it, because nobody ever chose prettier
for this directory — the script and the four-year-old `prettier@2` pin
came with the template. No CI job invoked it. It gated nothing, and a
check nobody can pass is a check nobody reads.

Configuring around it does not work either: sweeping `printWidth`
∈ {80, 88, 90, 100} against `trailingComma` ∈ {none, es5, all} moves the
failure count between 29 and 32 files. The differences are not width —
they are hand-wrapping plus the absent trailing comma in multiline object
literals — so there is no setting under which this code is nearly
formatted. Running `--write` is +2503/−1085 lines across 31 files, every
one of which had been rewritten within the previous week, and it would
take `git blame` on that work with it.

So the script is gone rather than left red. The TypeScript here is
hand-formatted; match the file you are editing. Adopting a formatter
later is a fine decision — it is just one somebody should make on
purpose, with a `.prettierrc` recording it, at a moment when a whole-tree
rewrite is not landing on top of live security work.
