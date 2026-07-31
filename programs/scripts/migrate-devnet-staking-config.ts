/**
 * One-shot devnet layout migration for `StakingConfig`.
 *
 * The flat `unbonding_period_secs` became the per-role
 * `unbonding_period_secs_by_role` array (OFS-4100 §4 gives merchants,
 * arbitrators and everyone else different periods, which one field cannot
 * hold). That grows the account by 48 bytes, so from the moment the new
 * binary is deployed the live singleton no longer deserializes and every
 * instruction that loads it fails — `stake`, `request_unstake`,
 * `withdraw_unstaked`, `slash`, and governance's `cast_vote` through it.
 *
 * **That window is real and this script is what closes it.** Run it
 * immediately after the program upgrade, not later:
 *
 *   npx tsx scripts/migrate-devnet-staking-config.ts --dry-run
 *   npx tsx scripts/migrate-devnet-staking-config.ts
 *
 * The account is rewritten in place by the program's own
 * `migrate_staking_config`, never recreated: the config PDA and the two
 * token vaults created alongside it must keep their addresses, and the
 * stake vault holds real staked OPEN that a new config could not sign for.
 *
 * This migrates layout only. It carries `min_stake_by_role`, `slash_bps`
 * and all three authorities across untouched and asserts afterwards that
 * they did not move; the OFS-4100 §4 policy values are written separately
 * by `scripts/apply-devnet-staking-floors.ts`, through the validated
 * `update_staking_config` path that emits an event. A resize that also
 * restated policy would be a parameter write nobody could audit.
 *
 * A second run fails loudly with `AlreadyMigrated` rather than corrupting
 * anything: the instruction admits exactly one input length.
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

/** From `target/idl/staking.json`, not hand-computed. Unchanged by this
 *  rework: an Anchor discriminator is derived from the instruction name,
 *  not from its arguments. */
const MIGRATE_DISCRIMINATOR = Buffer.from([
  205, 116, 145, 145, 71, 211, 110, 126,
]);

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

const DAY = 24 * 60 * 60;

/**
 * The per-role unbonding periods OFS-4100 §4 signs off, indexed by `Role`.
 * Mirrors `staking::constants::RECOMMENDED_UNBONDING_PERIOD_SECS_BY_ROLE`.
 *
 * Every entry is passed explicitly rather than relying on the program's
 * "zero inherits the old flat value" fill. That fill exists so a partial
 * migration is possible; a migration meaning to set all seven should say
 * all seven, where a reviewer can see them.
 */
const UNBONDING_PERIOD_SECS_BY_ROLE = [
  BigInt(1 * DAY), // Merchant — 24 hours
  BigInt(3 * DAY), // Arbitrator — 3 days
  BigInt(7 * DAY), // NodeOperator
  BigInt(7 * DAY), // NotificationProvider
  BigInt(7 * DAY), // OracleProvider
  BigInt(7 * DAY), // RiskIntelligenceProvider
  BigInt(7 * DAY), // SnapshotProvider
];

/**
 * Both decoders below walk the account with a single moving cursor and
 * assert the walk consumes exactly the account length minus its three
 * trailing u8 bumps (OFS-4200 §7.1).
 *
 * Independently-computed per-field offsets are how this repo previously
 * decoded a `StakingConfig`, skipped a 32-byte field, and reported two
 * adjacent pubkeys with their identities transposed — well-formed,
 * plausible, and wrong, and it read as correct until the chain
 * contradicted it. A cursor that has to balance cannot skip a field
 * silently. The values printed are also individually recognisable
 * (604800s, 1000 bps, whole thousands of OPEN at 9 decimals), which
 * catches a transposition that a balanced walk alone would not.
 */
const TRAILING_BUMPS = 3; // bump, stake_vault_bump, rewards_vault_bump

function reader(data: Buffer) {
  let o = 8; // Anchor account discriminator.
  return {
    pubkey: () => {
      const v = new PublicKey(data.subarray(o, o + 32));
      o += 32;
      return v;
    },
    u64: () => {
      const v = data.readBigUInt64LE(o);
      o += 8;
      return v;
    },
    i64: () => {
      const v = data.readBigInt64LE(o);
      o += 8;
      return v;
    },
    u16: () => {
      const v = data.readUInt16LE(o);
      o += 2;
      return v;
    },
    balance: (label: string) => {
      const expected = data.length - TRAILING_BUMPS;
      if (o !== expected) {
        throw new Error(
          `${label} decode walked ${o} bytes, expected ${expected} ` +
            `(account ${data.length} less ${TRAILING_BUMPS} trailing bumps) — ` +
            `the field walk does not match the on-chain struct`,
        );
      }
    },
  };
}

/** The layout as deployed *before* this migration: one flat i64. */
function decodeBefore(data: Buffer) {
  const r = reader(data);
  const decoded = {
    admin: r.pubkey(),
    mint: r.pubkey(),
    minStakeByRole: Array.from({ length: ROLE_COUNT }, () => r.u64()),
    unbondingPeriodSecs: r.i64(),
    slashBps: r.u16(),
    slashingAuthority: r.pubkey(),
    slashDestination: r.pubkey(),
    rewardsAuthority: r.pubkey(),
  };
  r.balance("pre-migration");
  return decoded;
}

/** The layout after: seven i64s where the one used to be. */
function decodeAfter(data: Buffer) {
  const r = reader(data);
  const decoded = {
    admin: r.pubkey(),
    mint: r.pubkey(),
    minStakeByRole: Array.from({ length: ROLE_COUNT }, () => r.u64()),
    unbondingPeriodSecsByRole: Array.from({ length: ROLE_COUNT }, () => r.i64()),
    slashBps: r.u16(),
    slashingAuthority: r.pubkey(),
    slashDestination: r.pubkey(),
    rewardsAuthority: r.pubkey(),
  };
  r.balance("post-migration");
  return decoded;
}

function i64LE(value: bigint): Buffer {
  const b = Buffer.alloc(8);
  b.writeBigInt64LE(value);
  return b;
}

const DECIMALS = 1_000_000_000n; // OPEN has 9 decimals (OFS-4100 §1)

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

  const prev = decodeBefore(before.data);
  console.log(`  admin                 ${prev.admin.toBase58()}`);
  console.log(`  unbonding_period_secs ${prev.unbondingPeriodSecs}`);
  console.log(`  slash_bps             ${prev.slashBps}`);
  for (let i = 0; i < ROLE_COUNT; i++) {
    console.log(
      `  min_stake ${ROLE_NAMES[i]!.padEnd(26)} ${(
        prev.minStakeByRole[i]! / DECIMALS
      )
        .toString()
        .padStart(7)} OPEN`,
    );
  }

  if (!prev.admin.equals(admin.publicKey)) {
    throw new Error(
      `loaded wallet ${admin.publicKey.toBase58()} is not the config admin ${prev.admin.toBase58()}`,
    );
  }

  if (process.argv.includes("--dry-run")) {
    console.log(
      "  dry run — pre-migration layout decoded cleanly, nothing sent",
    );
    return;
  }

  const ix = new TransactionInstruction({
    programId: STAKING_PROGRAM_ID,
    keys: [
      { pubkey: admin.publicKey, isSigner: true, isWritable: true },
      { pubkey: stakingConfig, isSigner: false, isWritable: true },
      { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
    ],
    data: Buffer.concat([
      MIGRATE_DISCRIMINATOR,
      ...UNBONDING_PERIOD_SECS_BY_ROLE.map(i64LE),
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

  // Read it back and decode against the new layout rather than trusting
  // the transaction's success — the failure this guards is an account
  // that is the right size and holds the wrong bytes.
  const after = await connection.getAccountInfo(stakingConfig);
  if (!after) throw new Error("config vanished after migration");
  console.log(`  size after:  ${after.data.length} bytes`);
  const now = decodeAfter(after.data);

  for (let i = 0; i < ROLE_COUNT; i++) {
    console.log(
      `  unbonding ${ROLE_NAMES[i]!.padEnd(26)} ${now.unbondingPeriodSecsByRole[
        i
      ]!
        .toString()
        .padStart(7)}s`,
    );
  }

  const expected = UNBONDING_PERIOD_SECS_BY_ROLE;
  if (!now.unbondingPeriodSecsByRole.every((v, i) => v === expected[i])) {
    throw new Error("the per-role unbonding periods did not take");
  }

  // A layout migration must move nothing else. Checked field by decoded
  // field rather than by comparing raw tail bytes, because the tail is
  // exactly what shifted: only the decode would catch a 48-byte write
  // landing one field early.
  const untouched =
    now.admin.equals(prev.admin) &&
    now.mint.equals(prev.mint) &&
    now.slashBps === prev.slashBps &&
    now.slashingAuthority.equals(prev.slashingAuthority) &&
    now.slashDestination.equals(prev.slashDestination) &&
    now.rewardsAuthority.equals(prev.rewardsAuthority) &&
    now.minStakeByRole.every((v, i) => v === prev.minStakeByRole[i]);
  if (!untouched) {
    throw new Error(
      "a field other than the unbonding periods moved — a layout migration " +
        "must carry every other value across untouched",
    );
  }

  console.log(
    "  verified: per-role unbonding written, every other field unchanged",
  );
  console.log(
    "  next: scripts/apply-devnet-staking-floors.ts writes the OFS-4100 §4 policy",
  );
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
