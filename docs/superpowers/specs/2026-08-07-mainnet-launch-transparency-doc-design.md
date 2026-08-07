# Design — Mainnet launch-readiness register & buyer transparency doc

Status: **Draft for review** · Date: 2026-08-07 · Scope: `openfiat-core` (new
top-level `MAINNET-LAUNCH.md`)

## Problem

Preparing an OPEN token sale on mainnet needs a single authoritative document
that (a) tracks every go-live gate and its status for the launch operators, and
(b) is honest and legible enough to double as a **buyer-facing transparency
page** a prospective contributor can use to verify the sale is legitimate before
sending money. There is no such document today; the relevant facts are spread
across `ROADMAP.md`, `programs/README.md`, `devnet-addresses.json`, the
tokenomics spec, and the security assessment.

## Decision

One Markdown document, `openfiat-core/MAINNET-LAUNCH.md`, canonical in the core
repo (so it cannot drift from the program IDs, CI guardrail, findings, and gates
it cites), written in plain buyer-legible language, linked from `README.md`.
Audience: primarily buyer/public transparency, secondarily the operator gate
register. Confirmed with the maintainer (2026-08-07).

### Non-negotiable honesty constraints

- **Leads with an unmissable status banner:** the sale is **NOT live**, the
  protocol is on **devnet only**, and a mainnet sale will not open until an
  external security audit and final tokenomics sign-off are complete. Nothing in
  the doc is an offer to sell securities.
- **No overstatement of readiness.** Every gate shows its true state; open items
  read as open. Consistent with the SDKs' "not audited for production/mainnet"
  disclaimer and `ROADMAP.md`'s audit gate.
- **No "invest now" / price-appreciation / returns language.**

### Deliberately excluded (would mislead or leak)

- No private-key file paths, no infra IPs, no internal host addresses.
- No named audit firm or audit date until a real engagement exists.
- No mainnet program IDs or a mainnet mint address until they actually exist
  (shown as `TBD — set at deploy`).

## Document structure

Top banner (above), then:

1. **Status at a glance — the gate register.** A table lanes 2–5 tick off:
   external audit ⬜ · tokenomics sign-off (OFS-4100 currently draft) ⬜ · keys
   backed up off-machine ⬜ · multisig authority live (admin + upgrade +
   treasury) ⬜ · release artifacts signed ⬜ · open assessment findings closed
   ⬜ · presale claim-anytime/sweep change shipped & devnet-proven ⬜. Each row:
   what it means, current state, where it's tracked.
2. **Token facts (verifiable on-chain).** 1,000,000,000 OPEN, 9 decimals, mint
   **and** freeze authority permanently revoked (fixed supply; no one can mint
   or freeze). Cite the devnet mint; mainnet mint = TBD.
3. **Allocation.** The 7 buckets with real percentages: Community Presale 20% ·
   AllenHark Treasury 14% · Ecosystem Treasury 17% · Infrastructure Bootstrap
   12% · Community Incentives 17% · Liquidity Programs 12% · Strategic Reserve
   8%. Note the presale bucket (200M OPEN) is the only supply the sale sells.
4. **Sale terms.** Reflects the sub-project-5 model: 1 OPEN = 1 USDC; sale
   allocation = the 200M Community Presale bucket; pay in USDC or
   SOL/whitelisted stablecoins via atomic Jupiter swap; per-wallet min and max
   (max = 10,000,000 USDC-equivalent); **no vesting**; **claim your OPEN anytime
   after buying**; **all purchases are final — there is no refund**; proceeds are
   swept to the multisig treasury on an ongoing basis. The no-refund term is
   stated plainly, not buried.
5. **How the sale works & how to verify.** contribute → entitlement accrues 1:1
   → claim (anytime). Program id + source links so a buyer can read the exact
   code custodying their money; the `sweep_proceeds` destination is a fixed,
   published treasury.
6. **Custody & authority.** The Squads-multisig model (published multisig
   address once it exists; **no key locations**), and the program upgrade
   authority. Populated by sub-project 2.
7. **Security posture.** Audit status (pending); the adversarial assessment and
   which findings are closed vs open (sub-project 3); the
   `security@openfiat.network` disclosure path from `SECURITY.md`.
8. **Go-live sequence & rollback.** The public-safe deploy → init → verify steps
   and what happens if a step fails. Populated by sub-project 4.

## Maintenance model

The gate register is the live tracking surface: sub-projects 2–5 update their
rows and sections as they complete, so at any moment the doc reflects true
readiness. A row flips to ✅ only when its lane is genuinely done and pushed.

## Verification

- `MAINNET-LAUNCH.md` exists at repo root, linked from `README.md`.
- Banner present and unambiguous; no excluded item leaks (grep for key paths,
  infra IPs, "audited", mainnet-beta).
- Every numeric claim (supply, decimals, allocations, caps, price) matches
  `programs/scripts/genesis.ts` and the presale `SaleConfig` fields.
- Every gate row cross-references a real tracker (ROADMAP item, sub-project,
  CI guardrail).

## Out of scope

- Rendering the doc on the `openfiat-docs` site (a trivial later follow-on if
  broader buyer reach is wanted).
- Resolving the tokenomics sign-off or the audit themselves — the doc *tracks*
  these gates, it does not close them.
