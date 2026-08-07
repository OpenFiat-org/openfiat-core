# Design — Presale: claim-anytime + `sweep_proceeds` (mainnet model)

Status: **Draft for review** · Date: 2026-08-07 · Scope: `programs/programs/presale`

## Problem

Two operational needs surfaced ahead of a mainnet OPEN sale:

1. **Buyers want their OPEN immediately after buying**, so they can stake and
   run node instances without waiting for the sale to close. Today OPEN is only
   claimable after `finalize_sale`, which cannot run until `end_time` passes or
   the 200M hard cap is reached — potentially weeks.
2. **The team needs to draw down USDC proceeds during the sale** to fund
   operations. Today contributions are locked in `usdc_vault` until
   `finalize_sale` sweeps them, and finalize is terminal (it ends the sale), so
   there is no way to fund ops while the sale is still running.

## Decision

Adopt a **claim-anytime + continuous-sweep** model, made sound by removing the
refund path. Confirmed with the maintainer (2026-08-07).

This is a change to a money-handling program and **expands the external-audit
surface**; it must be explicitly listed in scope for the audit gate
(`ROADMAP.md`, `programs/README.md`). No mainnet deploy happens under this spec
— the deliverable is a devnet-proven program plus tests.

## Current flow (baseline)

- `contribute_usdc` / `contribute_with_swap`: buyer pays USDC (or swaps
  SOL/whitelisted stablecoin → USDC atomically), a `Contribution` PDA accrues
  `amount_usdc` and `open_entitlement` (1:1, decimal-scaled). **No OPEN moves.**
- `finalize_sale` (admin, `has_one = admin`): after `end_time` or hard cap,
  if `total_raised >= soft_cap` sweeps `usdc_vault → treasury` and sets
  `Finalized`; else sets `SoftCapMissed`.
- `claim` (buyer): requires `Finalized`, transfers `open_entitlement` from
  `presale_vault → buyer` once.
- `refund` (buyer): requires `SoftCapMissed`, returns USDC.

## Target flow

### 1. `claim` — ungated from finalize

Remove the `require!(state == Finalized, SaleNotFinalized)` gate. A buyer may
claim their accrued OPEN while the sale is `Active` (or `Finalized`). Guards
retained: `has_one = buyer`, `!contribution.claimed`, canonical PDA seeds.

`claim` transfers `contribution.open_entitlement` — the buyer's **cumulative**
entitlement. Because a wallet may contribute again after claiming, `claim` must
pay only the *unclaimed* portion. Replace the boolean `claimed` with a
`claimed_open: u64` high-water mark on `Contribution`:

- `claim` pays `open_entitlement - claimed_open`, then sets
  `claimed_open = open_entitlement`.
- A repeat `claim` with nothing newly accrued pays 0 (reject with
  `NothingToClaim` rather than emit a zero transfer).

This makes contribute → claim → contribute-more → claim-more correct, which the
finalize-gated model never had to handle.

### 2. New instruction `sweep_proceeds`

Admin-gated draw-down of USDC proceeds while the sale runs.

- Accounts: `admin: Signer` (`has_one = admin` on `sale_config`);
  `sale_config` (mut); `usdc_vault` (mut, `== sale_config.usdc_vault`);
  `treasury` (mut, **`== sale_config.treasury`**); `usdc_mint`
  (`== sale_config.usdc_mint`); `token_program`.
- Args: `amount: u64`. Require `1 <= amount <= usdc_vault.amount`.
- Effect: `transfer_checked(usdc_vault → treasury, amount)` signed by the
  `SaleConfig` PDA (seeds `[SALE_CONFIG_SEED, sale_nonce]`), the same authority
  finalize uses. Allowed while `state == Active` (finalize handles any residual
  at close).
- Emits `event ProceedsSwept { sale_config, treasury, amount, vault_remaining }`
  so buyers can watch proceeds move on-chain.

**Trust guardrails (load-bearing, documented for buyers):**
- Destination is the **fixed configured treasury**, enforced by constraint —
  admin controls *when* to sweep, never *where* the funds go.
- `admin` and the treasury owner must be the **Squads multisig**
  (sub-project 2). "Admin can sweep anytime" is only acceptable because no
  single key can do it. This makes sub-projects 1 (transparency) and 2
  (multisig) prerequisites, not optional.

### 3. Remove the refund path (soundness)

Claim-anytime is sound **iff refunds are impossible** — otherwise a buyer could
claim OPEN *and* refund USDC (double-dip). Make this structural:

- Delete the `refund` instruction and the `SoftCapMissed` variant of
  `SaleState` (leaving `Active`, `Finalized`).
- `initialize_sale` rejects `soft_cap != 0` (`ErrorCode::SoftCapNotSupported`),
  so a sale that could ever be refundable cannot be created.
- Remove now-dead `refund`-related error codes; keep `Overflow`, `Unauthorized`,
  etc.

### 4. `finalize_sale` simplification

With no `SoftCapMissed`, finalize is: require ended (`now > end_time ||
total_raised >= hard_cap`), sweep any residual `usdc_vault → treasury`, set
`Finalized`. Its remaining purpose is to be the signal that stops further
`contribute`s (which require `Active`).

## Invariants (must hold; assert in tests)

- **No oversell / vault drain.** `sum(open_entitlement)` over all contributions
  ≤ `hard_cap` (enforced by the existing `total_raised <= hard_cap` check in
  `contribute`), and `presale_vault` holds exactly `hard_cap` OPEN at 1:1, so
  concurrent claims can never exceed the vault balance.
- **Claims survive sweeps.** `sweep_proceeds` only moves USDC out of
  `usdc_vault`; it never touches `presale_vault`. Every accrued OPEN
  entitlement remains fully claimable after any sweep.
- **No double-claim.** `claimed_open` is monotonic and never exceeds
  `open_entitlement`.
- **No refund path.** No instruction returns USDC to a buyer; `soft_cap` is
  forced to 0 at init.

## State / account changes

- `Contribution.claimed: bool` → `Contribution.claimed_open: u64`. This changes
  the account layout. Acceptable because the mainnet program is a **fresh
  deploy** (no existing mainnet `Contribution` accounts to migrate). Devnet
  accounts from the old layout are disposable test state; the devnet re-proof
  runs against a freshly initialized sale.
- `SaleState`: drop `SoftCapMissed`.

## Tests (`programs/tests/presale.ts`)

Add / adjust:
- Claim during `Active` succeeds and delivers the right OPEN amount.
- contribute → claim → contribute-more → claim-more: second claim pays only the
  newly accrued delta; a third claim with no new contribution rejects with
  `NothingToClaim`.
- `sweep_proceeds` by a non-admin signer rejects (`Unauthorized`).
- `sweep_proceeds` to an account other than `sale_config.treasury` rejects.
- `sweep_proceeds` of `amount > vault balance` rejects; a valid partial sweep
  leaves the vault reduced by exactly `amount` and emits `ProceedsSwept`.
- After a sweep, a pending buyer can still claim their full entitlement (the
  claims-survive-sweeps invariant).
- `initialize_sale` with `soft_cap != 0` rejects.
- No `refund` instruction exists in the IDL.

## Devnet proof (before any mainnet consideration)

Rebuild and redeploy the presale program to devnet (in-place upgrade on the
existing program id `75rJ9MRAaSnAc8tg4AfeTFVDCVrN6jdD5CqeyE4UoUw7`), initialize
a fresh sale, and run a script proving: contribute → claim-while-active →
sweep_proceeds → contribute-more → claim-more, asserting balances and the
invariants above. Record the signatures in `CONFORMANCE.md` alongside the
existing presale proofs.

## Out of scope

- Instant settlement inside `contribute` (folding the OPEN transfer into the
  buy tx) — considered and set aside in favor of the two-step claim-anytime
  model.
- Accepting/holding raw SOL as proceeds — all non-USDC contributions are
  swapped to USDC at contribution time; proceeds stay USDC-denominated.
- The multisig itself (sub-project 2) and the mainnet deploy/init
  (sub-project 4).

## Ripple effects on sibling sub-projects

- **#1 (transparency doc):** "Sale terms" states — claim your OPEN anytime after
  buying; all sales final; no refunds; proceeds swept to the multisig treasury
  on an ongoing basis.
- **#3 (audit):** audit scope explicitly lists claim-anytime + `sweep_proceeds`
  + refund removal.
- **#4 (deploy params):** `initialize_sale` carries `soft_cap = 0`, multisig
  `admin`, multisig-owned `treasury`.
