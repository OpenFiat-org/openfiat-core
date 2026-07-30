/**
 * Runs the account migrations that the deployed program upgrade requires.
 *
 * Both instructions grow a live account in place by resize alone, which only
 * works because each new field was appended after `bump` rather than placed
 * in declaration order — every pre-existing offset keeps its meaning, so an
 * un-migrated account still decodes correctly until it is resized.
 *
 *   escrow  migrate_fee_config      FeeConfig     203 -> 726 bytes
 *   staking migrate_stake_account   StakeAccount   82 ->  90 bytes  (x2)
 *
 * Both are permissionless and idempotent-by-size: this script checks the
 * current length first and skips an account that is already migrated, so a
 * re-run is a no-op rather than an error.
 *
 * Sizes are asserted against the expected targets afterwards by reading the
 * accounts back. A migration that silently resizes to the wrong length would
 * otherwise surface much later as a deserialization failure in an unrelated
 * instruction.
 *
 * Usage: npx ts-node scripts/run-devnet-migrations.ts [--commit]
 */
import * as fs from "fs";
import * as path from "path";
import * as crypto from "crypto";
import {
  Connection,
  Keypair,
  PublicKey,
  SystemProgram,
  Transaction,
  TransactionInstruction,
  clusterApiUrl,
} from "@solana/web3.js";

const ESCROW = new PublicKey("HaPpM1QYM3dKp3sX7zhEdft9hB6ncu6xfALAbkyQChQP");
const STAKING = new PublicKey("HYEXk8XQukBkZbiYB33JyVefQDxqyCpPudad3wBCyYmx");

const FEE_CONFIG_TARGET = 726;
const STAKE_ACCOUNT_TARGET = 90;

/** The two pre-migration StakeAccounts, found by enumerating the staking
 *  program's accounts and filtering on the old 82-byte length. */
const STAKE_ACCOUNTS = [
  "9ia8NzrdLm3gDn4RPJnNNxfwDGzHXTkN3B9ceLVz4vXR",
  "ED4nijxrULzC1fTuSM6hoNNMJFWXE7uSLiuY2GwhsQhu",
];

const discriminator = (name: string) =>
  crypto.createHash("sha256").update(`global:${name}`).digest().subarray(0, 8);

interface Step {
  label: string;
  programId: PublicKey;
  ixName: string;
  account: PublicKey;
  target: number;
}

async function main() {
  const commit = process.argv.includes("--commit");
  const connection = new Connection(
    process.env.SOLANA_RPC_URL || clusterApiUrl("devnet"),
    "confirmed",
  );
  const keypairPath =
    process.env.SOLANA_KEYPAIR ||
    path.join(process.env.HOME || "~", ".config/solana/id.json");
  const payer = Keypair.fromSecretKey(
    Uint8Array.from(JSON.parse(fs.readFileSync(keypairPath, "utf-8"))),
  );

  const [feeConfig] = PublicKey.findProgramAddressSync(
    [Buffer.from("fee_config")],
    ESCROW,
  );

  const steps: Step[] = [
    {
      label: "FeeConfig",
      programId: ESCROW,
      ixName: "migrate_fee_config",
      account: feeConfig,
      target: FEE_CONFIG_TARGET,
    },
    ...STAKE_ACCOUNTS.map((a, i) => ({
      label: `StakeAccount ${i + 1}`,
      programId: STAKING,
      ixName: "migrate_stake_account",
      account: new PublicKey(a),
      target: STAKE_ACCOUNT_TARGET,
    })),
  ];

  for (const step of steps) {
    const before = await connection.getAccountInfo(step.account);
    if (!before) {
      throw new Error(`${step.label} ${step.account.toBase58()} does not exist`);
    }
    if (before.data.length >= step.target) {
      console.log(
        `${step.label.padEnd(16)} ${step.account.toBase58()} already ${before.data.length} bytes — skipping`,
      );
      continue;
    }

    console.log(
      `${step.label.padEnd(16)} ${step.account.toBase58()} ${before.data.length} -> ${step.target}`,
    );

    const ix = new TransactionInstruction({
      programId: step.programId,
      keys: [
        { pubkey: payer.publicKey, isSigner: true, isWritable: true },
        { pubkey: step.account, isSigner: false, isWritable: true },
        { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
      ],
      data: discriminator(step.ixName),
    });

    const { blockhash } = await connection.getLatestBlockhash();
    const tx = new Transaction({
      feePayer: payer.publicKey,
      recentBlockhash: blockhash,
    }).add(ix);

    const sim = await connection.simulateTransaction(tx);
    if (sim.value.err) {
      console.error("  simulation failed:", sim.value.err);
      console.error((sim.value.logs || []).join("\n"));
      process.exit(1);
    }
    console.log("  simulation: ok");

    if (!commit) continue;

    tx.sign(payer);
    const signature = await connection.sendRawTransaction(tx.serialize());
    await connection.confirmTransaction(signature, "confirmed");
    console.log("  signature:", signature);

    const after = await connection.getAccountInfo(step.account);
    const len = after?.data.length ?? -1;
    if (len !== step.target) {
      throw new Error(
        `${step.label} is ${len} bytes after migration, expected ${step.target}`,
      );
    }
    console.log(`  verified: ${len} bytes`);
  }

  if (!commit) {
    console.log("\ndry run — nothing sent. Re-run with --commit to apply.");
  } else {
    console.log("\nall migrations verified at their target sizes.");
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
