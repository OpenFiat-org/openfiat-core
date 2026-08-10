/**
 * OPEN token genesis script (OFS-4100 §1-2, OFS-4200 §3).
 *
 * Creates the fixed-supply Token-2022 mint, mints the entire 100,000,000,000
 * OPEN supply once, permanently revokes mint AND freeze authority, then
 * distributes the supply across the 7 allocation buckets per OFS-4100 §2.
 *
 * The Community Presale bucket is transferred into a PDA owned by the
 * `openfiat-presale` program (seed: "presale_vault") so that program can
 * authorize `claim` transfers out of it in Phase 3 without a human keypair
 * ever custodying presale-bound tokens. The other 6 buckets are, for this
 * devnet phase, each given their own dedicated placeholder keypair identity
 * (persisted under scripts/.bucket-keys/, gitignored) so every bucket has a
 * genuinely distinct on-chain token account — associated-token-accounts are
 * derived solely from (owner, mint), so reusing one owner pubkey across
 * multiple "buckets" would silently collapse them into a single shared
 * account. This is still a clearly-flagged placeholder: production custody
 * for these (a real multisig such as Squads, and/or the openfiat-vesting
 * program for time-locked buckets) is out of scope until the phases that
 * actually consume them (staking rewards, governance-authorized treasury
 * spend, etc.) are built.
 *
 * Usage:
 *   npx ts-node scripts/genesis.ts --cluster localnet
 *   npx ts-node scripts/genesis.ts --cluster devnet
 */
import * as fs from "fs";
import * as path from "path";
import {
  Connection,
  Keypair,
  PublicKey,
  clusterApiUrl,
} from "@solana/web3.js";
import {
  TOKEN_2022_PROGRAM_ID,
  createMint,
  mintTo,
  getOrCreateAssociatedTokenAccount,
  setAuthority,
  AuthorityType,
  transfer,
} from "@solana/spl-token";

const DECIMALS = 6; // OFS-4100 §1 (re-baselined 2026-08-09; was 9)
const TOTAL_SUPPLY = 100_000_000_000n; // OFS-4100 §1 (re-baselined 2026-08-09; was 1_000_000_000n)

// OFS-4100 §2 — Community Presale is [CONFIRMED] at 20% (the full bucket
// funds two sequential sale phases — presale then public sale — rather than
// sizing a single capped raise); the other six are still
// [PROPOSED — NEEDS SIGN-OFF], expressed as basis points of TOTAL_SUPPLY so
// the split is exact regardless of decimals.
const BUCKETS: Array<{ name: string; bps: number }> = [
  { name: "community-presale", bps: 2000 }, // 20%
  { name: "allenhark-treasury", bps: 1400 }, // 14%
  { name: "ecosystem-treasury", bps: 1700 }, // 17%
  { name: "infrastructure-bootstrap", bps: 1200 }, // 12%
  { name: "community-incentives", bps: 1700 }, // 17%
  { name: "liquidity-programs", bps: 1200 }, // 12%
  { name: "strategic-reserve", bps: 800 }, // 8%
];

function bucketAmount(bps: number): bigint {
  return (TOTAL_SUPPLY * BigInt(bps)) / 10_000n;
}

/**
 * Every amount this script ever hands to an on-chain u64 field (the mint's
 * total base-unit supply, and each bucket's base-unit share) must fit u64.
 * At 6 decimals the total is 1e17 — comfortably under u64::MAX (~1.8e19) —
 * but this is re-checked here rather than assumed, so a future decimals or
 * supply change fails loudly before any mint/transfer instead of silently
 * wrapping on-chain.
 */
function assertU64Bounds(): void {
  const U64_MAX = (1n << 64n) - 1n;
  const totalBase = TOTAL_SUPPLY * 10n ** BigInt(DECIMALS);
  if (totalBase > U64_MAX) {
    throw new Error(`total supply ${totalBase} base units exceeds u64::MAX`);
  }
  for (const bucket of BUCKETS) {
    const base = bucketAmount(bucket.bps) * 10n ** BigInt(DECIMALS);
    if (base > U64_MAX) {
      throw new Error(
        `bucket "${bucket.name}" ${base} base units exceeds u64::MAX`,
      );
    }
  }
}

function parseArgs() {
  const args = process.argv.slice(2);
  const idx = args.indexOf("--cluster");
  const cluster = idx >= 0 ? args[idx + 1] : "localnet";
  return { cluster };
}

const BUCKET_KEYS_DIR = path.join(__dirname, ".bucket-keys");

/**
 * Load-or-create a dedicated placeholder keypair for a non-presale bucket.
 * Persisted so repeated runs (e.g. re-verifying) reuse the same owner
 * instead of minting a fresh identity every time.
 */
function loadOrCreateBucketKeypair(name: string): Keypair {
  fs.mkdirSync(BUCKET_KEYS_DIR, { recursive: true });
  const keyPath = path.join(BUCKET_KEYS_DIR, `${name}.json`);
  if (fs.existsSync(keyPath)) {
    return Keypair.fromSecretKey(
      Uint8Array.from(JSON.parse(fs.readFileSync(keyPath, "utf-8"))),
    );
  }
  const kp = Keypair.generate();
  fs.writeFileSync(keyPath, JSON.stringify(Array.from(kp.secretKey)));
  return kp;
}

async function main() {
  assertU64Bounds();

  const { cluster } = parseArgs();
  if (cluster === "mainnet-beta" || cluster === "mainnet") {
    throw new Error(
      "Refusing to run genesis against mainnet — this workspace is devnet-only until the audit gate clears (see OFS-4200 §Status Banner).",
    );
  }

  const endpoint =
    cluster === "localnet"
      ? "http://127.0.0.1:8899"
      : clusterApiUrl(cluster as "devnet" | "testnet");
  const connection = new Connection(endpoint, "confirmed");

  const keypairPath =
    process.env.SOLANA_KEYPAIR ||
    path.join(process.env.HOME || "~", ".config/solana/id.json");
  const admin = Keypair.fromSecretKey(
    Uint8Array.from(JSON.parse(fs.readFileSync(keypairPath, "utf-8"))),
  );
  console.log(`Cluster:      ${cluster} (${endpoint})`);
  console.log(`Admin pubkey: ${admin.publicKey.toBase58()}`);

  console.log("\nCreating Token-2022 mint...");
  const mint = await createMint(
    connection,
    admin,
    admin.publicKey, // mint authority (temporary — revoked below)
    admin.publicKey, // freeze authority (temporary — revoked below)
    DECIMALS,
    undefined,
    undefined,
    TOKEN_2022_PROGRAM_ID,
  );
  console.log(`Mint: ${mint.toBase58()}`);

  console.log("\nMinting full fixed supply to a temporary holding account...");
  const holding = await getOrCreateAssociatedTokenAccount(
    connection,
    admin,
    mint,
    admin.publicKey,
    false,
    undefined,
    undefined,
    TOKEN_2022_PROGRAM_ID,
  );
  const totalUnits = TOTAL_SUPPLY * 10n ** BigInt(DECIMALS);
  await mintTo(
    connection,
    admin,
    mint,
    holding.address,
    admin,
    totalUnits,
    [],
    undefined,
    TOKEN_2022_PROGRAM_ID,
  );

  console.log("\nDistributing to allocation buckets (OFS-4100 §2)...");
  const [presaleVaultPda] = PublicKey.findProgramAddressSync(
    [Buffer.from("presale_vault")],
    new PublicKey("7KaEpDzZuqye1xqqp3RnvBJXnDxbU3W9zVrUr5vBS2fU"), // presale program ID
  );

  const addresses: Record<string, string> = {
    mint: mint.toBase58(),
    admin: admin.publicKey.toBase58(),
  };

  // The public devnet RPC (api.devnet.solana.com) rate-limits bursts of
  // requests harder than mainnet or a paid provider — a brief pause between
  // buckets avoids tripping that limit and failing mid-genesis.
  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

  for (const bucket of BUCKETS) {
    const amount = bucketAmount(bucket.bps) * 10n ** BigInt(DECIMALS);
    const isPresale = bucket.name === "community-presale";
    const owner = isPresale
      ? presaleVaultPda
      : loadOrCreateBucketKeypair(bucket.name).publicKey;
    const account = await getOrCreateAssociatedTokenAccount(
      connection,
      admin,
      mint,
      owner,
      isPresale, // allowOwnerOffCurve (PDA) — the other 6 owners are real keypairs, always on-curve
      undefined,
      undefined,
      TOKEN_2022_PROGRAM_ID,
    );
    await sleep(cluster === "localnet" ? 0 : 800);
    await transfer(
      connection,
      admin,
      holding.address,
      account.address,
      admin,
      amount,
      [],
      undefined,
      TOKEN_2022_PROGRAM_ID,
    );
    console.log(
      `  ${bucket.name.padEnd(26)} ${(bucket.bps / 100).toFixed(0).padStart(3)}%  owner=${owner.toBase58()}  account=${account.address.toBase58()}`,
    );
    addresses[`bucket_${bucket.name}_owner`] = owner.toBase58();
    addresses[`bucket_${bucket.name}`] = account.address.toBase58();
    await sleep(cluster === "localnet" ? 0 : 800);
  }

  console.log(
    "\nRevoking mint authority and freeze authority permanently (OFS-4100 §1)...",
  );
  await setAuthority(
    connection,
    admin,
    mint,
    admin,
    AuthorityType.MintTokens,
    null,
    [],
    undefined,
    TOKEN_2022_PROGRAM_ID,
  );
  await setAuthority(
    connection,
    admin,
    mint,
    admin,
    AuthorityType.FreezeAccount,
    null,
    [],
    undefined,
    TOKEN_2022_PROGRAM_ID,
  );

  const outPath = path.join(__dirname, "..", "devnet-addresses.json");
  const existing = fs.existsSync(outPath)
    ? JSON.parse(fs.readFileSync(outPath, "utf-8"))
    : {};
  fs.writeFileSync(
    outPath,
    JSON.stringify({ ...existing, [cluster]: addresses }, null, 2) + "\n",
  );
  console.log(`\nGenesis complete. Addresses written to ${outPath}`);
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
