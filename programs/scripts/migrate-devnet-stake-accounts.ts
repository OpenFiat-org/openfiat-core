/**
 * One-shot devnet migration for the `StakeAccount` layout change that added
 * `first_staked_at` — the clock behind OFS-4100 §4's arbitrator stake-age
 * requirement.
 *
 * The field was appended after `bump`, so a pre-migration account still
 * decodes correctly for every field that existed before; what it cannot do
 * is be deserialized by the *program*, which now expects eight more bytes.
 * Any instruction taking a `StakeAccount` therefore fails against an
 * unmigrated account until this has run.
 *
 * Run after redeploying the staking program:
 *   npx ts-node scripts/migrate-devnet-stake-accounts.ts
 *
 * Discovers every account to migrate itself, by asking the cluster for
 * program accounts carrying the `StakeAccount` discriminator at the
 * pre-migration size. Already-migrated accounts are 90 bytes and so are
 * never returned — which is what makes a second run a no-op rather than
 * something that could reset an age clock.
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
import { join } from "node:path";

const RPC_URL = process.env.SOLANA_RPC_URL ?? "https://api.devnet.solana.com";
const STAKING_PROGRAM_ID = new PublicKey(
  "HYEXk8XQukBkZbiYB33JyVefQDxqyCpPudad3wBCyYmx",
);

/** Pre-migration `StakeAccount` size; 90 once `first_staked_at` is there. */
const OLD_LEN = 82;
const NEW_LEN = 90;

const ROLE_NAMES = [
  "Merchant",
  "Arbitrator",
  "NodeOperator",
  "NotificationProvider",
  "OracleProvider",
  "RiskIntelligenceProvider",
  "SnapshotProvider",
];

/**
 * Both discriminators are read out of the real `anchor build`-generated IDL
 * rather than hardcoded here. Every other script in this directory pastes
 * the bytes in with a comment saying where they came from, which works
 * until an instruction is renamed and the paste silently keeps pointing at
 * the old one. Reading the IDL cannot drift.
 */
function fromIdl(): { instruction: Buffer; account: Buffer } {
  const idlPath = join(__dirname, "..", "target", "idl", "staking.json");
  let idl: {
    instructions: { name: string; discriminator: number[] }[];
    accounts: { name: string; discriminator: number[] }[];
  };
  try {
    idl = JSON.parse(readFileSync(idlPath, "utf8"));
  } catch {
    throw new Error(`could not read ${idlPath} — run \`anchor build\` first`);
  }
  const ix = idl.instructions.find((i) => i.name === "migrate_stake_account");
  if (!ix) {
    throw new Error(
      "staking.json has no `migrate_stake_account` — the deployed program " +
        "predates this migration, so redeploy before running it",
    );
  }
  const account = idl.accounts.find((a) => a.name === "StakeAccount");
  if (!account) throw new Error("staking.json has no `StakeAccount`");
  return {
    instruction: Buffer.from(ix.discriminator),
    account: Buffer.from(account.discriminator),
  };
}

async function main() {
  const { instruction: MIGRATE_DISCRIMINATOR, account: ACCOUNT_DISCRIMINATOR } =
    fromIdl();
  const connection = new Connection(RPC_URL, "confirmed");
  const payer = Keypair.fromSecretKey(
    Uint8Array.from(
      JSON.parse(
        readFileSync(
          process.env.ANCHOR_WALLET ?? `${homedir()}/.config/solana/id.json`,
          "utf8",
        ),
      ),
    ),
  );

  const stale = await connection.getProgramAccounts(STAKING_PROGRAM_ID, {
    filters: [
      { dataSize: OLD_LEN },
      { memcmp: { offset: 0, bytes: ACCOUNT_DISCRIMINATOR.toString("base64"), encoding: "base64" } },
    ],
  });

  if (stale.length === 0) {
    console.log("no pre-migration stake accounts found — nothing to do");
    return;
  }
  console.log(`${stale.length} stake account(s) to migrate\n`);

  let migrated = 0;
  for (const { pubkey, account } of stale) {
    const owner = new PublicKey(account.data.subarray(8, 40));
    const role = ROLE_NAMES[account.data[40]!] ?? `role ${account.data[40]}`;
    const amount = account.data.readBigUInt64LE(41);
    console.log(`${pubkey.toBase58()}  ${role}  owner ${owner.toBase58()}`);
    console.log(`  staked ${amount} base units`);

    const ix = new TransactionInstruction({
      programId: STAKING_PROGRAM_ID,
      keys: [
        { pubkey: payer.publicKey, isSigner: true, isWritable: true },
        { pubkey, isSigner: false, isWritable: true },
        { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
      ],
      data: MIGRATE_DISCRIMINATOR,
    });

    const { blockhash, lastValidBlockHeight } =
      await connection.getLatestBlockhash();
    const tx = new Transaction({
      feePayer: payer.publicKey,
      blockhash,
      lastValidBlockHeight,
    }).add(ix);
    tx.sign(payer);
    const signature = await connection.sendRawTransaction(tx.serialize());
    await connection.confirmTransaction(
      { signature, blockhash, lastValidBlockHeight },
      "confirmed",
    );

    // Read back and check the bytes rather than trusting that the
    // transaction succeeding means the account is now what we expect.
    const after = await connection.getAccountInfo(pubkey);
    if (!after) throw new Error(`${pubkey.toBase58()} vanished`);
    if (after.data.length !== NEW_LEN) {
      throw new Error(
        `${pubkey.toBase58()} is ${after.data.length} bytes after migration, expected ${NEW_LEN}`,
      );
    }
    const firstStakedAt = after.data.readBigInt64LE(OLD_LEN);
    // An account holding stake must come out with a running clock, and one
    // holding none must come out with a zero clock. Either way round being
    // wrong would mean the age gate is reading a number that does not mean
    // what it says.
    if (amount > 0n && firstStakedAt === 0n) {
      throw new Error(`${pubkey.toBase58()} holds stake but has a zero age clock`);
    }
    if (amount === 0n && firstStakedAt !== 0n) {
      throw new Error(`${pubkey.toBase58()} holds no stake but has a running age clock`);
    }
    console.log(
      `  migrated in ${signature}; first_staked_at ${firstStakedAt}` +
        (firstStakedAt === 0n
          ? " (no stake held)"
          : ` (${new Date(Number(firstStakedAt) * 1000).toISOString()})`),
    );
    migrated++;
  }

  console.log(`\n${migrated} account(s) migrated`);
  const remaining = await connection.getProgramAccounts(STAKING_PROGRAM_ID, {
    filters: [
      { dataSize: OLD_LEN },
      { memcmp: { offset: 0, bytes: ACCOUNT_DISCRIMINATOR.toString("base64"), encoding: "base64" } },
    ],
  });
  if (remaining.length > 0) {
    throw new Error(`${remaining.length} account(s) still unmigrated`);
  }
  console.log("no pre-migration stake accounts remain");
}

main().catch((err) => {
  console.error(err);
  process.exitCode = 1;
});
