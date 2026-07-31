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
 * ALREADY APPLIED on devnet (see devnet-addresses.json). Kept because the
 * decode below is the same walk every later script reuses, and because a
 * one-shot whose offsets are allowed to rot is a landmine for whoever runs
 * it next — it has been updated for the per-role unbonding layout.
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

const ROLE_COUNT = 7;
const TRAILING_BUMPS = 3; // bump, stake_vault_bump, rewards_vault_bump

/**
 * A single moving cursor over `StakingConfig`, asserting the walk lands
 * exactly at the account length minus its trailing bumps (OFS-4200 §7.1).
 *
 * This used to read each field at its own independently-computed offset.
 * That is the pattern that, elsewhere in this repo and on this exact
 * struct, skipped a 32-byte field and reported two adjacent pubkeys with
 * their identities transposed — well-formed, plausible, and wrong. It also
 * silently survived `unbonding_period_secs` becoming a per-role array,
 * which shifted every field after it by 48 bytes: the offsets still
 * computed, and every value after the array would have been read out of
 * the wrong place. A cursor that has to balance cannot do either.
 */
function decode(data: Buffer) {
  let o = 8; // Anchor account discriminator.
  const pubkey = () => {
    const v = new PublicKey(data.subarray(o, o + 32));
    o += 32;
    return v;
  };
  const u64 = () => {
    const v = data.readBigUInt64LE(o);
    o += 8;
    return v;
  };
  const i64 = () => {
    const v = data.readBigInt64LE(o);
    o += 8;
    return v;
  };
  const u16 = () => {
    const v = data.readUInt16LE(o);
    o += 2;
    return v;
  };

  const decoded = {
    admin: pubkey(),
    mint: pubkey(),
    minStakeByRole: Array.from({ length: ROLE_COUNT }, () => u64()),
    unbondingPeriodSecsByRole: Array.from({ length: ROLE_COUNT }, () => i64()),
    slashBps: u16(),
    slashingAuthority: pubkey(),
    slashDestination: pubkey(),
    rewardsAuthority: pubkey(),
  };

  const expected = data.length - TRAILING_BUMPS;
  if (o !== expected) {
    throw new Error(
      `decode walked ${o} bytes, expected ${expected} (account ${data.length} ` +
        `less ${TRAILING_BUMPS} trailing bumps) — the field walk does not ` +
        `match the on-chain struct`,
    );
  }
  return decoded;
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
    ...prev.unbondingPeriodSecsByRole.map((secs) => {
      const b = Buffer.alloc(8);
      b.writeBigInt64LE(secs);
      return b;
    }),
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
  console.log(
    `  unbonding_period_secs    [${now.unbondingPeriodSecsByRole.join(", ")}]`,
  );

  const unchanged =
    now.slashBps === prev.slashBps &&
    now.unbondingPeriodSecsByRole.every(
      (v, i) => v === prev.unbondingPeriodSecsByRole[i],
    ) &&
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
