/**
 * One-shot devnet correction for the two `StakingConfig` fields that made
 * real instructions permanently unexecutable.
 *
 * `slash_destination` held a wallet, but `slash` requires that key to
 * deserialize as a Token-2022 `TokenAccount`, so no slash could ever run.
 * `rewards_authority` held an address nobody has a keypair for, and
 * `distribute_reward` requires it to sign, so no reward could ever be
 * distributed however correct the computed schedule was.
 *
 * Run after upgrading the staking program:
 *   npx ts-node scripts/fix-devnet-staking-authorities.ts
 *
 * Every numeric parameter is read off the live account and written back
 * unchanged. This script corrects two addresses; it is deliberately not a
 * way to quietly restate policy, and it prints a diff so a reviewer can
 * see that nothing else moved.
 */
import {
  Connection,
  Keypair,
  PublicKey,
  Transaction,
  TransactionInstruction,
} from "@solana/web3.js";
import { readFileSync } from "node:fs";
import { homedir } from "node:os";

const RPC_URL = process.env.SOLANA_RPC_URL ?? "https://api.devnet.solana.com";
const STAKING_PROGRAM_ID = new PublicKey(
  "HYEXk8XQukBkZbiYB33JyVefQDxqyCpPudad3wBCyYmx",
);
const MINT = new PublicKey("29w8TroBTYoaqrXBDcpv5L54VZRA8Kf7kU5U1cakvFdj");

/**
 * The emergency reserve owner's Token-2022 ATA for the OPEN mint — the
 * mint the stake vault itself holds, so a transfer out of it can land
 * here. See devnet-addresses.json for why the emergency reserve rather
 * than an operational budget.
 */
const SLASH_DESTINATION = new PublicKey(
  "Aq5schhgFHNFkMyTLuGkCSg4MGYbxV1DbnHS6cK4RKGu",
);

/** From `target/idl/staking.json`, not hand-computed. */
const DISCRIMINATOR = Buffer.from([214, 238, 91, 123, 207, 114, 9, 246]);

/** `8 disc + 32 admin + 32 mint`, then the fields in declaration order. */
const OFF_MIN_STAKE = 72;
const OFF_UNBONDING = OFF_MIN_STAKE + 7 * 8;
const OFF_SLASH_BPS = OFF_UNBONDING + 8;
const OFF_SLASHING_AUTHORITY = OFF_SLASH_BPS + 2;
const OFF_SLASH_DESTINATION = OFF_SLASHING_AUTHORITY + 32;
const OFF_REWARDS_AUTHORITY = OFF_SLASH_DESTINATION + 32;

function decode(data: Buffer) {
  return {
    minStakeByRole: Array.from({ length: 7 }, (_, i) =>
      data.readBigUInt64LE(OFF_MIN_STAKE + i * 8),
    ),
    unbondingPeriodSecs: data.readBigInt64LE(OFF_UNBONDING),
    slashBps: data.readUInt16LE(OFF_SLASH_BPS),
    slashingAuthority: new PublicKey(
      data.subarray(OFF_SLASHING_AUTHORITY, OFF_SLASHING_AUTHORITY + 32),
    ),
    slashDestination: new PublicKey(
      data.subarray(OFF_SLASH_DESTINATION, OFF_SLASH_DESTINATION + 32),
    ),
    rewardsAuthority: new PublicKey(
      data.subarray(OFF_REWARDS_AUTHORITY, OFF_REWARDS_AUTHORITY + 32),
    ),
  };
}

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
  const prev = decode(before.data);
  console.log(`staking_config ${stakingConfig.toBase58()}`);
  console.log(`  slash_destination before ${prev.slashDestination.toBase58()}`);
  console.log(`  rewards_authority before ${prev.rewardsAuthority.toBase58()}`);

  // The slashing authority is already a key we hold, so it is carried over
  // rather than reassigned — only the two broken fields change.
  const data = Buffer.concat([
    DISCRIMINATOR,
    ...prev.minStakeByRole.map(u64LE),
    (() => {
      const b = Buffer.alloc(8);
      b.writeBigInt64LE(prev.unbondingPeriodSecs);
      return b;
    })(),
    (() => {
      const b = Buffer.alloc(2);
      b.writeUInt16LE(prev.slashBps);
      return b;
    })(),
    prev.slashingAuthority.toBuffer(),
    admin.publicKey.toBuffer(), // rewards_authority: the only key that exists
  ]);

  const ix = new TransactionInstruction({
    programId: STAKING_PROGRAM_ID,
    keys: [
      { pubkey: admin.publicKey, isSigner: true, isWritable: false },
      { pubkey: stakingConfig, isSigner: false, isWritable: true },
      { pubkey: MINT, isSigner: false, isWritable: false },
      { pubkey: SLASH_DESTINATION, isSigner: false, isWritable: false },
    ],
    data,
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
  console.log(`  updated in ${signature}`);

  // Decode the account back rather than trusting the transaction's success.
  const after = await connection.getAccountInfo(stakingConfig);
  if (!after) throw new Error("config vanished after update");
  const now = decode(after.data);
  console.log(`  slash_destination after  ${now.slashDestination.toBase58()}`);
  console.log(`  rewards_authority after  ${now.rewardsAuthority.toBase58()}`);
  console.log(`  slashing_authority       ${now.slashingAuthority.toBase58()}`);
  console.log(`  slash_bps                ${now.slashBps}`);
  console.log(`  unbonding_period_secs    ${now.unbondingPeriodSecs}`);

  const unchanged =
    now.slashBps === prev.slashBps &&
    now.unbondingPeriodSecs === prev.unbondingPeriodSecs &&
    now.slashingAuthority.equals(prev.slashingAuthority) &&
    now.minStakeByRole.every((v, i) => v === prev.minStakeByRole[i]);
  if (!unchanged) throw new Error("a policy field moved; expected only the two addresses to change");
  if (!now.slashDestination.equals(SLASH_DESTINATION))
    throw new Error("slash_destination did not take");
  if (!now.rewardsAuthority.equals(admin.publicKey))
    throw new Error("rewards_authority did not take");
  console.log("  verified: both addresses corrected, every policy field unchanged");
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
