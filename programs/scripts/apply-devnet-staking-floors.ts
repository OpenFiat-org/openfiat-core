/**
 * Writes OFS-4100 §4's signed-off staking policy onto the devnet
 * `StakingConfig`: the 500-OPEN Merchant/Arbitrator floors and the 5%
 * slash rate.
 *
 * Run AFTER `scripts/migrate-devnet-staking-config.ts`, which grows the
 * account into the per-role-unbonding layout this script decodes. Running
 * it first fails on the balance check rather than writing anything.
 *
 *   npx tsx scripts/apply-devnet-staking-floors.ts --dry-run
 *   npx tsx scripts/apply-devnet-staking-floors.ts
 *
 * # Why the arbitrator floor may be lowered at all
 *
 * 10,000 OPEN was the only thing standing between an attacker and every
 * seat on a dispute: seats went to whoever called `commit_dispute_vote`
 * first, so capture cost seven staked wallets. Dropping to 500 without
 * anything else changing would have made that twenty times cheaper.
 *
 * What makes it safe is that per-case arbitrator sortition has landed —
 * `openfiat_programs_shared::sortition::qualifies_for_seat`, called from
 * `escrow::commit_dispute_vote`. Eligibility for a specific case is now a
 * draw against a per-case seed, so assembling a majority needs many aged,
 * funded wallets rather than seven of any size. The barrier moved from the
 * size of one stake to the number of independent ones, which is the thing
 * a per-wallet minimum could never price.
 *
 * NOTE that `FeeConfig.arbitrator_sortition_bps` ships at zero — disabled —
 * and is switched on by governance through `update_fee_config`. The code
 * path is live; the parameter is not. Check it before treating the floor
 * as safe on any cluster carrying real value.
 *
 * # Discipline
 *
 * `update_staking_config` rewrites the whole config, so every field this
 * script does not mean to change is read off the live account and echoed
 * back — never restated as a constant here, which would silently revert
 * whatever the chain actually holds. Afterwards the account is decoded
 * again and each field is asserted to have moved, or not, as intended.
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

/** From `target/idl/staking.json`, not hand-computed. */
const UPDATE_DISCRIMINATOR = Buffer.from([214, 238, 91, 123, 207, 114, 9, 246]);

const ROLE_COUNT = 7;
const ROLE_NAMES = [
  "Merchant",
  "Arbitrator",
  "NodeOperator",
  "NotificationProvider",
  "OracleProvider",
  "RiskIntelligenceProvider",
  "SnapshotProvider",
];

const DECIMALS = 1_000_000_000n; // OPEN has 9 decimals (OFS-4100 §1)
const DAY = 24 * 60 * 60;

/** Mirrors `staking::constants::RECOMMENDED_MIN_STAKE_BY_ROLE`. Merchant
 *  and Arbitrator are floors that scale above; the rest are flat. */
const TARGET_MIN_STAKE_BY_ROLE = [
  500n * DECIMALS,
  500n * DECIMALS,
  1_000n * DECIMALS,
  5_000n * DECIMALS,
  1_000n * DECIMALS,
  1_000n * DECIMALS,
  1_000n * DECIMALS,
];

/** Mirrors `staking::constants::RECOMMENDED_UNBONDING_PERIOD_SECS_BY_ROLE`.
 *  Restated so this script asserts the migration wrote what it meant to,
 *  rather than accepting whatever it finds. */
const TARGET_UNBONDING_PERIOD_SECS_BY_ROLE = [
  BigInt(1 * DAY),
  BigInt(3 * DAY),
  BigInt(7 * DAY),
  BigInt(7 * DAY),
  BigInt(7 * DAY),
  BigInt(7 * DAY),
  BigInt(7 * DAY),
];

/** Mirrors `staking::constants::RECOMMENDED_SLASH_BPS` — 5%, down from 10%. */
const TARGET_SLASH_BPS = 500;

const TRAILING_BUMPS = 3; // bump, stake_vault_bump, rewards_vault_bump

/**
 * A single moving cursor, asserting the walk lands exactly at the account
 * length minus its trailing bumps (OFS-4200 §7.1). Per-field offsets
 * computed independently are how a previous decode of this very struct
 * skipped a 32-byte field and transposed two pubkeys — well-formed and
 * wrong. A cursor that has to balance cannot skip a field silently.
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
        `less ${TRAILING_BUMPS} trailing bumps) — either the field walk does ` +
        `not match the on-chain struct, or migrate-devnet-staking-config.ts ` +
        `has not been run yet`,
    );
  }
  return decoded;
}

function u16LE(value: number): Buffer {
  const b = Buffer.alloc(2);
  b.writeUInt16LE(value);
  return b;
}
function u64LE(value: bigint): Buffer {
  const b = Buffer.alloc(8);
  b.writeBigUInt64LE(value);
  return b;
}
function i64LE(value: bigint): Buffer {
  const b = Buffer.alloc(8);
  b.writeBigInt64LE(value);
  return b;
}

function report(label: string, config: ReturnType<typeof decode>) {
  console.log(`  ${label}`);
  for (let i = 0; i < ROLE_COUNT; i++) {
    console.log(
      `    ${ROLE_NAMES[i]!.padEnd(26)} min ${(
        config.minStakeByRole[i]! / DECIMALS
      )
        .toString()
        .padStart(6)} OPEN   unbonding ${config.unbondingPeriodSecsByRole[i]!
        .toString()
        .padStart(7)}s`,
    );
  }
  console.log(`    slash_bps ${config.slashBps}`);
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
  console.log(`  account size ${before.data.length} bytes`);
  report("before", prev);

  if (!prev.admin.equals(admin.publicKey)) {
    throw new Error(
      `loaded wallet ${admin.publicKey.toBase58()} is not the config admin ${prev.admin.toBase58()}`,
    );
  }
  if (!prev.mint.equals(MINT)) {
    throw new Error(
      `config mint is ${prev.mint.toBase58()}, expected ${MINT.toBase58()}`,
    );
  }

  if (process.argv.includes("--dry-run")) {
    console.log("  dry run — decoded cleanly, no transaction sent");
    return;
  }

  // The two authorities and the slash destination are carried over from
  // the live account, not restated: this instruction rewrites the whole
  // config, and they were themselves the subject of an earlier correction.
  const data = Buffer.concat([
    UPDATE_DISCRIMINATOR,
    ...TARGET_MIN_STAKE_BY_ROLE.map(u64LE),
    ...TARGET_UNBONDING_PERIOD_SECS_BY_ROLE.map(i64LE),
    u16LE(TARGET_SLASH_BPS),
    prev.slashingAuthority.toBuffer(),
    prev.rewardsAuthority.toBuffer(),
  ]);

  const ix = new TransactionInstruction({
    programId: STAKING_PROGRAM_ID,
    keys: [
      { pubkey: admin.publicKey, isSigner: true, isWritable: false },
      { pubkey: stakingConfig, isSigner: false, isWritable: true },
      { pubkey: MINT, isSigner: false, isWritable: false },
      { pubkey: prev.slashDestination, isSigner: false, isWritable: false },
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

  const after = await connection.getAccountInfo(stakingConfig);
  if (!after) throw new Error("config vanished after update");
  const now = decode(after.data);
  report("after", now);

  const took =
    now.minStakeByRole.every((v, i) => v === TARGET_MIN_STAKE_BY_ROLE[i]) &&
    now.unbondingPeriodSecsByRole.every(
      (v, i) => v === TARGET_UNBONDING_PERIOD_SECS_BY_ROLE[i],
    ) &&
    now.slashBps === TARGET_SLASH_BPS;
  if (!took) throw new Error("the OFS-4100 §4 values did not take");

  const addressesUnchanged =
    now.admin.equals(prev.admin) &&
    now.mint.equals(prev.mint) &&
    now.slashingAuthority.equals(prev.slashingAuthority) &&
    now.slashDestination.equals(prev.slashDestination) &&
    now.rewardsAuthority.equals(prev.rewardsAuthority);
  if (!addressesUnchanged) {
    throw new Error(
      "an address moved; this script changes policy figures only",
    );
  }

  console.log(
    "  verified: §4 floors, per-role unbonding and 5% slash applied; " +
      "every address unchanged",
  );
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
