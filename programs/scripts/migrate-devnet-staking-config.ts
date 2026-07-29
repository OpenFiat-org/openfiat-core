/**
 * One-shot devnet migration for the `StakingConfig` layout change.
 *
 * `min_stake` + `min_stake_arbitrator` became `min_stake_by_role`, which
 * grows the account past its allocated size, so the live singleton must be
 * rewritten by the program's own `migrate_staking_config` instruction
 * before anything can deserialize it again.
 *
 * Run after redeploying the staking program:
 *   npx ts-node scripts/migrate-devnet-staking-config.ts
 *
 * Idempotent in the sense that a second run fails loudly with
 * `AlreadyMigrated` rather than corrupting anything.
 */
import {
  Connection,
  Keypair,
  PublicKey,
  SystemProgram,
  Transaction,
  TransactionInstruction,
} from "@solana/web3.js";
import { readFileSync } from "node:fs";
import { homedir } from "node:os";

const RPC_URL = process.env.SOLANA_RPC_URL ?? "https://api.devnet.solana.com";
const STAKING_PROGRAM_ID = new PublicKey(
  "HYEXk8XQukBkZbiYB33JyVefQDxqyCpPudad3wBCyYmx",
);

/** From `target/idl/staking.json`, not hand-computed. */
const MIGRATE_DISCRIMINATOR = Buffer.from([
  205, 116, 145, 145, 71, 211, 110, 126,
]);

const DECIMALS = 1_000_000_000n; // OPEN has 9 decimals (OFS-4100 §1)

/**
 * Indexed by `Role`: Merchant, Arbitrator, NodeOperator,
 * NotificationProvider, OracleProvider, RiskIntelligenceProvider,
 * SnapshotProvider.
 *
 * Notification gateways are set to 5,000 OPEN by protocol-steward decision.
 * Everything else keeps the OFS-4100 §4 figures the deployment already had:
 * 1,000 flat, 10,000 for arbitrators.
 */
const MIN_STAKE_BY_ROLE = [
  1_000n * DECIMALS,
  10_000n * DECIMALS,
  1_000n * DECIMALS,
  5_000n * DECIMALS,
  1_000n * DECIMALS,
  1_000n * DECIMALS,
  1_000n * DECIMALS,
];

function u64LE(value: bigint): Buffer {
  const b = Buffer.alloc(8);
  b.writeBigUInt64LE(value);
  return b;
}

async function main() {
  const connection = new Connection(RPC_URL, "confirmed");
  const admin = Keypair.fromSecretKey(
    Uint8Array.from(
      JSON.parse(
        readFileSync(
          process.env.ANCHOR_WALLET ?? `${homedir()}/.config/solana/id.json`,
          "utf8",
        ),
      ),
    ),
  );

  const [stakingConfig] = PublicKey.findProgramAddressSync(
    [Buffer.from("staking_config")],
    STAKING_PROGRAM_ID,
  );

  const before = await connection.getAccountInfo(stakingConfig);
  if (!before) throw new Error(`no staking config at ${stakingConfig}`);
  console.log(`staking_config ${stakingConfig.toBase58()}`);
  console.log(`  size before: ${before.data.length} bytes`);

  const ix = new TransactionInstruction({
    programId: STAKING_PROGRAM_ID,
    keys: [
      { pubkey: admin.publicKey, isSigner: true, isWritable: true },
      { pubkey: stakingConfig, isSigner: false, isWritable: true },
      { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
    ],
    data: Buffer.concat([
      MIGRATE_DISCRIMINATOR,
      ...MIN_STAKE_BY_ROLE.map(u64LE),
    ]),
  });

  const { blockhash, lastValidBlockHeight } =
    await connection.getLatestBlockhash();
  const tx = new Transaction({
    feePayer: admin.publicKey,
    blockhash,
    lastValidBlockHeight,
  }).add(ix);
  tx.sign(admin);
  const signature = await connection.sendRawTransaction(tx.serialize());
  await connection.confirmTransaction(
    { signature, blockhash, lastValidBlockHeight },
    "confirmed",
  );
  console.log(`  migrated in ${signature}`);

  // Read it back and decode against the new layout rather than trusting the
  // transaction's success.
  const after = await connection.getAccountInfo(stakingConfig);
  if (!after) throw new Error("config vanished after migration");
  console.log(`  size after:  ${after.data.length} bytes`);
  const names = [
    "Merchant",
    "Arbitrator",
    "NodeOperator",
    "NotificationProvider",
    "OracleProvider",
    "RiskIntelligenceProvider",
    "SnapshotProvider",
  ];
  for (let i = 0; i < names.length; i++) {
    const raw = after.data.readBigUInt64LE(72 + i * 8);
    console.log(
      `  ${names[i]!.padEnd(26)} ${(raw / DECIMALS).toString().padStart(7)} OPEN`,
    );
  }
  const tail = 72 + names.length * 8;
  console.log(`  unbonding_period_secs ${after.data.readBigInt64LE(tail)}`);
  console.log(`  slash_bps             ${after.data.readUInt16LE(tail + 8)}`);
}

main().catch((err) => {
  console.error(err);
  process.exitCode = 1;
});
