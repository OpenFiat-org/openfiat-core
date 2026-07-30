/**
 * Brings a live devnet presale's economic bounds in line with OFS-4100 §3,
 * via the admin-only `update_sale_params` instruction.
 *
 * # Why this script hand-encodes the instruction
 *
 * The presale program's **published IDL is stale**: `anchor idl upgrade` was
 * never run after `update_sale_params` was added, so the on-chain IDL account
 * lists only six instructions and `anchor.Program` therefore has no
 * `.updateSaleParams()` method to call. The instruction itself *is* deployed
 * — verified by simulating it against devnet, which reached
 * `Instruction: UpdateSaleParams` and succeeded, while a deliberately
 * nonexistent discriminator fell through to Anchor's error 101
 * (`InstructionFallbackNotFound`). So the wire format is encoded directly
 * here, the same way `openfiat-sdks` encodes every instruction it builds.
 *
 * Republishing the IDL is worth doing, but it needs a rebuild and is not a
 * prerequisite for this fix.
 *
 * # Why every field is read before it is written
 *
 * `update_sale_params` **replaces all six** of its fields — it is not a patch.
 * `end_time` and `max_slippage_bps` are not being changed here, so they are
 * read from the live account and passed straight back. Omitting them, or
 * passing a plausible-looking constant, would silently move the sale's
 * closing date.
 *
 * Usage: SALE_NONCE=1 npx ts-node scripts/update-devnet-sale-params.ts [--commit]
 * Without `--commit` the script simulates and prints the diff, changing nothing.
 */
import * as fs from "fs";
import * as path from "path";
import * as crypto from "crypto";
import {
  Connection,
  Keypair,
  PublicKey,
  Transaction,
  TransactionInstruction,
  clusterApiUrl,
} from "@solana/web3.js";

const PRESALE_PROGRAM_ID = new PublicKey(
  "75rJ9MRAaSnAc8tg4AfeTFVDCVrN6jdD5CqeyE4UoUw7",
);
const SALE_NONCE = BigInt(process.env.SALE_NONCE ?? "1");
const USDC_DECIMALS = 6n;
const USDC = (whole: bigint) => whole * 10n ** USDC_DECIMALS;

/**
 * OFS-4100 §3, all `[CONFIRMED]`, except `maxContribution` — the spec's table
 * says 1,000,000 USDC and the protocol steward has since raised it to
 * 10,000,000. OFS-4100 must be amended to match; a deployment that disagrees
 * with its own signed-off spec is the drift this project keeps closing.
 *
 * `softCap` is 0 because the spec says there is **no** soft cap: "there is no
 * minimum to raise, so there is no threshold a raise can fall short of and no
 * refund condition derived from one." Zero is how that is expressed on chain —
 * `finalize_sale` then always resolves to `Finalized`, never `SoftCapMissed`,
 * which is exactly the stated intent.
 *
 * `hardCap` is the full Community Presale bucket (200,000,000 OPEN at
 * 1 OPEN = 1 USDC). It must not exceed the bucket: entitlements are minted
 * 1:1 against contributions and `claim` pays out of a vault holding exactly
 * 200,000,000 OPEN, so a higher cap would let the sale promise OPEN that the
 * vault cannot deliver.
 */
const TARGET = {
  hardCap: USDC(200_000_000n),
  softCap: 0n,
  maxContribution: USDC(10_000_000n),
};

const u64 = (v: bigint) => {
  const b = Buffer.alloc(8);
  b.writeBigUInt64LE(v);
  return b;
};
const i64 = (v: bigint) => {
  const b = Buffer.alloc(8);
  b.writeBigInt64LE(v);
  return b;
};
const u16 = (v: number) => {
  const b = Buffer.alloc(2);
  b.writeUInt16LE(v);
  return b;
};

const discriminator = (name: string) =>
  crypto.createHash("sha256").update(`global:${name}`).digest().subarray(0, 8);

/**
 * Field offsets for `SaleConfig`, taken from the on-chain IDL's own field
 * order (which was cross-checked against `presale/src/state.rs`): an 8-byte
 * discriminator, then seven pubkeys, then the numeric bounds.
 */
const OFF = {
  admin: 8,
  hardCap: 232,
  softCap: 240,
  minContribution: 248,
  maxContribution: 256,
  maxSlippageBps: 264,
  startTime: 268,
  endTime: 276,
};

interface LiveConfig {
  admin: PublicKey;
  hardCap: bigint;
  softCap: bigint;
  minContribution: bigint;
  maxContribution: bigint;
  maxSlippageBps: number;
  startTime: bigint;
  endTime: bigint;
}

function decodeSaleConfig(data: Buffer): LiveConfig {
  return {
    admin: new PublicKey(data.subarray(OFF.admin, OFF.admin + 32)),
    hardCap: data.readBigUInt64LE(OFF.hardCap),
    softCap: data.readBigUInt64LE(OFF.softCap),
    minContribution: data.readBigUInt64LE(OFF.minContribution),
    maxContribution: data.readBigUInt64LE(OFF.maxContribution),
    maxSlippageBps: data.readUInt16LE(OFF.maxSlippageBps),
    startTime: data.readBigInt64LE(OFF.startTime),
    endTime: data.readBigInt64LE(OFF.endTime),
  };
}

const asUsdc = (base: bigint) => `${base / 10n ** USDC_DECIMALS} USDC`;

async function main() {
  const commit = process.argv.includes("--commit");
  const connection = new Connection(
    process.env.SOLANA_RPC_URL || clusterApiUrl("devnet"),
    "confirmed",
  );
  const keypairPath =
    process.env.SOLANA_KEYPAIR ||
    path.join(process.env.HOME || "~", ".config/solana/id.json");
  const admin = Keypair.fromSecretKey(
    Uint8Array.from(JSON.parse(fs.readFileSync(keypairPath, "utf-8"))),
  );

  const [saleConfig] = PublicKey.findProgramAddressSync(
    [Buffer.from("sale_config"), u64(SALE_NONCE)],
    PRESALE_PROGRAM_ID,
  );

  const account = await connection.getAccountInfo(saleConfig);
  if (!account) {
    throw new Error(
      `no sale_config at ${saleConfig.toBase58()} for nonce ${SALE_NONCE} — ` +
        `check SALE_NONCE (the live devnet sale is nonce 1, not 0)`,
    );
  }
  const before = decodeSaleConfig(Buffer.from(account.data));

  if (!before.admin.equals(admin.publicKey)) {
    throw new Error(
      `this sale's admin is ${before.admin.toBase58()} but the loaded keypair ` +
        `is ${admin.publicKey.toBase58()} — update_sale_params would fail ` +
        `Unauthorized`,
    );
  }

  console.log(`sale_config ${saleConfig.toBase58()} (nonce ${SALE_NONCE})`);
  console.log("before:", {
    hardCap: asUsdc(before.hardCap),
    softCap: asUsdc(before.softCap),
    minContribution: asUsdc(before.minContribution),
    maxContribution: asUsdc(before.maxContribution),
    maxSlippageBps: before.maxSlippageBps,
    endTime: new Date(Number(before.endTime) * 1000).toISOString(),
  });

  // `min_contribution` and `end_time` and `max_slippage_bps` are carried
  // through unchanged — see this file's header on why they must be resent.
  const args = Buffer.concat([
    u64(TARGET.hardCap),
    u64(TARGET.softCap),
    u64(before.minContribution),
    u64(TARGET.maxContribution),
    u16(before.maxSlippageBps),
    i64(before.endTime),
  ]);

  const ix = new TransactionInstruction({
    programId: PRESALE_PROGRAM_ID,
    keys: [
      { pubkey: admin.publicKey, isSigner: true, isWritable: false },
      { pubkey: saleConfig, isSigner: false, isWritable: true },
    ],
    data: Buffer.concat([
      discriminator("update_sale_params"),
      u64(SALE_NONCE),
      args,
    ]),
  });

  console.log("after (target):", {
    hardCap: asUsdc(TARGET.hardCap),
    softCap: asUsdc(TARGET.softCap),
    minContribution: asUsdc(before.minContribution) + " (unchanged)",
    maxContribution: asUsdc(TARGET.maxContribution),
    maxSlippageBps: before.maxSlippageBps + " (unchanged)",
    endTime:
      new Date(Number(before.endTime) * 1000).toISOString() + " (unchanged)",
  });

  const { blockhash } = await connection.getLatestBlockhash();
  const tx = new Transaction({
    feePayer: admin.publicKey,
    recentBlockhash: blockhash,
  }).add(ix);

  const sim = await connection.simulateTransaction(tx);
  if (sim.value.err) {
    console.error("simulation failed:", sim.value.err);
    console.error((sim.value.logs || []).join("\n"));
    process.exit(1);
  }
  console.log("simulation: ok");

  if (!commit) {
    console.log("\ndry run — nothing sent. Re-run with --commit to apply.");
    return;
  }

  tx.sign(admin);
  const signature = await connection.sendRawTransaction(tx.serialize());
  await connection.confirmTransaction(signature, "confirmed");
  console.log("signature:", signature);

  // Read the account back rather than trusting that the transaction
  // succeeded — the point is the resulting state, not the submission.
  const afterAccount = await connection.getAccountInfo(saleConfig);
  const after = decodeSaleConfig(Buffer.from(afterAccount!.data));
  console.log("verified on chain:", {
    hardCap: asUsdc(after.hardCap),
    softCap: asUsdc(after.softCap),
    minContribution: asUsdc(after.minContribution),
    maxContribution: asUsdc(after.maxContribution),
    maxSlippageBps: after.maxSlippageBps,
    endTime: new Date(Number(after.endTime) * 1000).toISOString(),
  });

  const mismatches: string[] = [];
  if (after.hardCap !== TARGET.hardCap) mismatches.push("hardCap");
  if (after.softCap !== TARGET.softCap) mismatches.push("softCap");
  if (after.maxContribution !== TARGET.maxContribution)
    mismatches.push("maxContribution");
  if (after.endTime !== before.endTime) mismatches.push("endTime MOVED");
  if (after.maxSlippageBps !== before.maxSlippageBps)
    mismatches.push("maxSlippageBps MOVED");
  if (after.minContribution !== before.minContribution)
    mismatches.push("minContribution MOVED");
  if (mismatches.length > 0) {
    throw new Error(`on-chain state does not match intent: ${mismatches.join(", ")}`);
  }
  console.log("all fields match intent; nothing unintended moved.");
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
