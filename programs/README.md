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
| `governance` | Not yet scaffolded (Phase 5b) | Proposals, voting, parameter updates, treasury spend |

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
deliberate, so CI and local iteration stay fast and deterministic. Actual
devnet deployment is a separate, explicit step:

```bash
# Requires devnet SOL in the deploying wallet — request via
# `solana airdrop 2` or a devnet faucet if rate-limited.
anchor deploy --provider.cluster devnet
```

After a real devnet deploy, record the resulting program ID in
`devnet-addresses.json` (created once the first program — `presale` — is
actually deployed; see Phase 3's exit criteria) — that file is the canonical
source every later phase (core wiring, SDKs, the app) reads from.

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

## Linting and license compliance

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo deny check   # uses this directory's own deny.toml
```
