/**
 * Genesis verification script (Phase 2 exit criteria).
 *
 * Reads back on-chain state for a cluster already processed by genesis.ts
 * and asserts: mint supply matches the fixed 100B, decimals = 6, mint and
 * freeze authority are both revoked (null), each bucket's balance matches
 * its bps share of total supply, and the sum of all 7 bucket balances
 * equals the total supply.
 *
 * Usage:
 *   npx ts-node scripts/verify-genesis.ts --cluster localnet
 */
import * as fs from "fs";
import * as path from "path";
import { Connection, PublicKey, clusterApiUrl } from "@solana/web3.js";
import { TOKEN_2022_PROGRAM_ID, getMint, getAccount } from "@solana/spl-token";

const DECIMALS = 6; // OFS-4100 §1 (re-baselined 2026-08-09; was 9)
const TOTAL_SUPPLY = 100_000_000_000n; // OFS-4100 §1 (re-baselined 2026-08-09; was 1_000_000_000n)
const TOTAL_SUPPLY_UNITS = TOTAL_SUPPLY * 10n ** BigInt(DECIMALS);

// Mirrors genesis.ts's BUCKETS bps exactly — must stay in lockstep so this
// script verifies what genesis.ts actually distributed, not a re-derivation
// that could silently drift from it.
const BUCKETS: Array<{ name: string; bps: number }> = [
  { name: "community-presale", bps: 2000 },
  { name: "allenhark-treasury", bps: 1400 },
  { name: "ecosystem-treasury", bps: 1700 },
  { name: "infrastructure-bootstrap", bps: 1200 },
  { name: "community-incentives", bps: 1700 },
  { name: "liquidity-programs", bps: 1200 },
  { name: "strategic-reserve", bps: 800 },
];

function bucketAmount(bps: number): bigint {
  return (TOTAL_SUPPLY * BigInt(bps)) / 10_000n;
}

function parseArgs() {
  const args = process.argv.slice(2);
  const idx = args.indexOf("--cluster");
  return { cluster: idx >= 0 ? args[idx + 1] : "localnet" };
}

function assert(cond: boolean, msg: string) {
  if (!cond) {
    throw new Error(`ASSERTION FAILED: ${msg}`);
  }
  console.log(`  ✓ ${msg}`);
}

async function main() {
  const { cluster } = parseArgs();
  const endpoint =
    cluster === "localnet"
      ? "http://127.0.0.1:8899"
      : clusterApiUrl(cluster as "devnet" | "testnet");
  const connection = new Connection(endpoint, "confirmed");

  const addrPath = path.join(__dirname, "..", "devnet-addresses.json");
  const all = JSON.parse(fs.readFileSync(addrPath, "utf-8"));
  const addresses = all[cluster];
  if (!addresses) {
    throw new Error(`No recorded addresses for cluster "${cluster}" in ${addrPath}`);
  }

  console.log(`Verifying genesis on ${cluster}...\n`);

  const mint = new PublicKey(addresses.mint);
  const mintInfo = await getMint(
    connection,
    mint,
    "confirmed",
    TOKEN_2022_PROGRAM_ID,
  );

  assert(mintInfo.decimals === DECIMALS, `decimals === ${DECIMALS}`);
  assert(
    mintInfo.supply === TOTAL_SUPPLY_UNITS,
    `total supply === 100,000,000,000 OPEN (${TOTAL_SUPPLY_UNITS} base units)`,
  );
  assert(mintInfo.mintAuthority === null, "mint authority is permanently revoked (null)");
  assert(mintInfo.freezeAuthority === null, "freeze authority is permanently revoked (null)");

  let bucketSum = 0n;
  const bucketAddrs: string[] = [];
  for (const bucket of BUCKETS) {
    const key = `bucket_${bucket.name}`;
    const addr = addresses[key];
    assert(!!addr, `devnet-addresses.json has an entry for ${key}`);
    bucketAddrs.push(addr);
    const account = await getAccount(
      connection,
      new PublicKey(addr),
      "confirmed",
      TOKEN_2022_PROGRAM_ID,
    );
    const expected = bucketAmount(bucket.bps) * 10n ** BigInt(DECIMALS);
    assert(
      account.amount === expected,
      `bucket "${bucket.name}" balance === ${bucket.bps / 100}% of total supply (${expected} base units)`,
    );
    bucketSum += account.amount;
  }
  assert(
    new Set(bucketAddrs).size === bucketAddrs.length,
    "all 7 bucket token accounts are distinct addresses (no shared/collapsed account)",
  );
  assert(
    bucketSum === TOTAL_SUPPLY_UNITS,
    "sum of all 7 bucket balances equals total supply (no tokens lost or double-counted)",
  );

  console.log("\nAll genesis invariants hold.");
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
