#!/usr/bin/env bash
# Stale-literal guard for the 2026-08-09 OPEN tokenomics re-baseline
# (OFS-4100 §1-4: 100,000,000,000 OPEN supply at 6 decimals, presale
# 1 USDC = 100 OPEN, public-sale phase-2 1 USDC = 80 OPEN).
#
# Docs and code comments must not ASSERT a pre-rebaseline figure (1B total
# supply, 200,000,000 OPEN presale bucket, "1 OPEN = 1 USDC" / 1:1 price,
# OPEN at nine decimals) as current fact. A file may still legitimately
# contain one of these strings if it is:
#   (a) clearly framed as history ("was 1:1", "superseded", "pre-rebaseline"),
#       or
#   (b) listed in ALLOWLIST below, with a reason.
# Anything else is a regression this guard fails on.
#
# Usage: scripts/check-stale-tokenomics-literals.sh
set -euo pipefail
cd "$(dirname "$0")/.."

# Files allowed to still contain pre-rebaseline OPEN figures.
ALLOWLIST=(
  # docs/superpowers/ (and .superpowers/) are gitignored SDD planning
  # artifacts, excluded from the scan entirely below rather than listed
  # here — they're dated working notes, not shipped documentation, and
  # some predate this very re-baseline by design.
  #
  # Devnet operational/proof scripts that describe the ACTUAL, still-live
  # pre-redeploy devnet cluster (old mint, 1B supply, 9 decimals, 1:1
  # price). Accurate today; will be swept as part of Task 4b's full
  # re-genesis ceremony, not this doc-only pass.
  "programs/scripts/extract-open-for-faucet.ts"
  "programs/scripts/prove-devnet-governance-vote.ts"
  "programs/scripts/prove-devnet-presale-claim-sweep.ts"
  "programs/scripts/set-devnet-governance-quorum.ts"
  "programs/scripts/migrate-devnet-staking-config.ts"
  "programs/scripts/stake-node-operator.ts"
  # Self-contained test fixtures with their own local OPEN_DECIMALS
  # constant, explicitly documented at the constant as NOT tracking the
  # live OPEN mint's decimals. Rewriting every derived base-unit assertion
  # in these files is a separate, larger change than this doc sweep.
  "crates/oracles/tests/fee_settlement.rs"
  "crates/registry/src/settlement.rs"
)

# Old-figure signatures. Each names OPEN or USDC explicitly so it can't
# false-positive on an unrelated ratio or SOL/wSOL's genuinely-correct nine
# decimals elsewhere in the workspace.
PATTERNS=(
  '1,000,000,000 OPEN'
  '200,000,000 OPEN'
  '1 OPEN = 1 USDC'
  'OPEN.{0,20}1:1'
  '1:1.{0,20}OPEN'
  'minted 1:1'
  'OPEN has 9 decimals'
  'OPEN.{0,40}(nine|9) decimal'
  '(nine|9) decimal.{0,40}OPEN'
)

is_allowed() {
  local file="$1"
  for a in "${ALLOWLIST[@]}"; do
    [ "$file" = "$a" ] && return 0
  done
  return 1
}

fail=0
for pattern in "${PATTERNS[@]}"; do
  while IFS= read -r line; do
    [ -z "$line" ] && continue
    file="${line%%:*}"
    file="${file#./}"
    if ! is_allowed "$file"; then
      echo "STALE TOKENOMICS LITERAL: $line"
      fail=1
    fi
  done < <(grep -rnE \
    --include='*.md' --include='*.rs' --include='*.ts' --include='*.tsx' \
    --exclude-dir=node_modules --exclude-dir=target --exclude-dir=.superpowers \
    --exclude-dir=superpowers --exclude-dir=.git --exclude-dir=.claude \
    -- "$pattern" . 2>/dev/null || true)
done

if [ "$fail" -ne 0 ]; then
  cat >&2 <<'EOF'

One or more files assert a pre-2026-08-09-rebaseline OPEN tokenomics figure
(1B supply, 200,000,000 OPEN presale bucket, 1:1 price, 9 decimals) as
current. Either update it to the re-baselined figure (100,000,000,000
supply, 100:1 presale rate / 80:1 phase-2, 6 decimals) or, if it is a
genuinely historical or out-of-scope reference, add it to ALLOWLIST in
this script with a reason.
EOF
  exit 1
fi

echo "OK: no stale pre-rebaseline tokenomics literals found outside the allowlist."
