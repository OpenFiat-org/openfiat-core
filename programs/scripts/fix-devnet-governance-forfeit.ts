/**
 * One-shot devnet correction for `GovernanceConfig.forfeit_destination`,
 * which held the ecosystem treasury *owner* wallet.
 *
 * `refund_or_forfeit_deposit` loads that field as a Token-2022
 * `TokenAccount` in its account context — unconditionally, before the
 * handler picks a branch. A wallet cannot deserialize as one, so the
 * instruction could never load its accounts and **neither** path worked:
 * not just forfeiture, but refunds too. A proposal's deposit could never
 * be settled either way. See OFS-4200 §7.
 *
 * Run after upgrading the governance program:
 *   npx tsx scripts/fix-devnet-governance-forfeit.ts
 *
 * The destination is the same owner's Token-2022 ATA for the OPEN mint.
 * Keeping that owner is deliberate: the stored wallet already recorded
 * where forfeited deposits were meant to go, so this fixes the type error
 * without quietly relocating protocol income. Every numeric parameter is
 * read off the live account and written back unchanged, and the script
 * re-decodes afterwards and fails if anything but the address moved.
 */
import {
  Connection,
  Keypair,
  PublicKey,
  Transaction,
  TransactionInstruction,
} from "@solana/web3.js";
import {
  TOKEN_2022_PROGRAM_ID,
  createAssociatedTokenAccountIdempotent,
} from "@solana/spl-token";
import { readFileSync } from "node:fs";
import { homedir } from "node:os";

const RPC_URL = process.env.SOLANA_RPC_URL ?? "https://api.devnet.solana.com";
const GOVERNANCE_PROGRAM_ID = new PublicKey(
  "AVJfKUjHsizkGGUy8sdz4Xma2hVgmgvgg8GmUMs8E4eE"
);
const MINT = new PublicKey("29w8TroBTYoaqrXBDcpv5L54VZRA8Kf7kU5U1cakvFdj");

/**
 * The ecosystem treasury owner's Token-2022 ATA for OPEN — the mint the
 * deposit vault holds, so a transfer out of it can actually land here.
 * The owner is unchanged from the wallet the config previously named.
 */
const FORFEIT_DESTINATION_OWNER = new PublicKey(
  "VueuQemTWZcZXMhfN1jRH1vs1zeWXYvzhDPC8uxJARF"
);
const FORFEIT_DESTINATION = new PublicKey(
  "H4Ghu6Q3MNzyX59pZ6L7jAjFFa2vR6YUdreLRN755ULF"
);

/** From `target/idl/governance.json`, not hand-computed. */
const DISCRIMINATOR = Buffer.from([140, 45, 181, 17, 77, 67, 157, 248]);

/**
 * Walks `GovernanceConfig` field by field with a single moving cursor,
 * rather than reading each field at an independently-computed offset.
 *
 * The distinction matters: with per-field constants, omitting a field
 * silently shifts every field after it, and the result still decodes into
 * well-formed pubkeys and plausible integers. That is not hypothetical —
 * a `StakingConfig` decode elsewhere in this repo skipped a 32-byte field
 * and reported two adjacent pubkeys with their identities swapped, which
 * read as correct and was published before the chain contradicted it.
 * A cursor cannot skip a field without failing the balance check below.
 * OFS-4200 §7.1.
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
  const u16 = () => {
    const v = data.readUInt16LE(o);
    o += 2;
    return v;
  };
  const i64 = () => {
    const v = data.readBigInt64LE(o);
    o += 8;
    return v;
  };

  const decoded = {
    admin: pubkey(),
    mint: pubkey(),
    totalOpenSupply: u64(),
    quorumBps: u16(),
    thresholdSimpleBps: u16(),
    thresholdTreasuryBps: u16(),
    thresholdUpgradeBps: u16(),
    quorumUpgradeBps: u16(),
    depositAmount: u64(),
    forfeitDestination: pubkey(),
    voteLockSecs: i64(),
  };

  // Everything above must consume the account exactly, less the two
  // trailing u8 bumps. If this does not balance, the walk is wrong —
  // not the account — and every value read above is suspect.
  const TRAILING_BUMPS = 2; // bump, deposit_vault_bump
  if (o !== data.length - TRAILING_BUMPS) {
    throw new Error(
      `decode walked ${o} bytes, expected ${data.length - TRAILING_BUMPS} ` +
        `(account ${data.length} less ${TRAILING_BUMPS} trailing bumps) — ` +
        `the field walk does not match the on-chain struct`
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

async function main() {
  const connection = new Connection(RPC_URL, "confirmed");
  const admin = Keypair.fromSecretKey(
    Uint8Array.from(
      JSON.parse(
        readFileSync(
          process.env.ANCHOR_WALLET ?? `${homedir()}/.config/solana/id.json`,
          "utf8"
        )
      )
    )
  );

  const [governanceConfig] = PublicKey.findProgramAddressSync(
    [Buffer.from("governance_config")],
    GOVERNANCE_PROGRAM_ID
  );

  const before = await connection.getAccountInfo(governanceConfig);
  if (!before) throw new Error(`no governance config at ${governanceConfig}`);
  const prev = decode(before.data);
  console.log(`governance_config ${governanceConfig.toBase58()}`);
  console.log(`  account size               ${before.data.length} bytes`);
  console.log(
    `  forfeit_destination before ${prev.forfeitDestination.toBase58()}`
  );

  // `--dry-run` proves the field walk balances against the live account
  // before any write happens. A decode that is wrong about the layout is
  // worth discovering here rather than after a transaction lands.
  if (process.argv.includes("--dry-run")) {
    console.log("  dry run — decoded cleanly, no transaction sent");
    console.table(
      Object.fromEntries(Object.entries(prev).map(([k, v]) => [k, String(v)]))
    );
    return;
  }

  // Idempotent: creates the ATA on a first run, no-ops on a re-run. The
  // instruction below would reject a non-existent destination anyway, so
  // doing it here keeps the whole correction to a single command.
  const created = await createAssociatedTokenAccountIdempotent(
    connection,
    admin,
    MINT,
    FORFEIT_DESTINATION_OWNER,
    { commitment: "confirmed" },
    TOKEN_2022_PROGRAM_ID
  );
  if (!created.equals(FORFEIT_DESTINATION)) {
    throw new Error(
      `derived ATA ${created.toBase58()} != expected ${FORFEIT_DESTINATION.toBase58()}`
    );
  }
  console.log(`  destination ATA ready ${created.toBase58()}`);

  const data = Buffer.concat([
    DISCRIMINATOR,
    u64LE(prev.totalOpenSupply),
    u16LE(prev.quorumBps),
    u16LE(prev.thresholdSimpleBps),
    u16LE(prev.thresholdTreasuryBps),
    u16LE(prev.thresholdUpgradeBps),
    u16LE(prev.quorumUpgradeBps),
    u64LE(prev.depositAmount),
    i64LE(prev.voteLockSecs),
  ]);

  const ix = new TransactionInstruction({
    programId: GOVERNANCE_PROGRAM_ID,
    keys: [
      { pubkey: admin.publicKey, isSigner: true, isWritable: false },
      { pubkey: governanceConfig, isSigner: false, isWritable: true },
      { pubkey: MINT, isSigner: false, isWritable: false },
      { pubkey: FORFEIT_DESTINATION, isSigner: false, isWritable: false },
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
    "confirmed"
  );
  console.log(`  updated in ${signature}`);

  // Decode the account back rather than trusting the transaction succeeded.
  const after = await connection.getAccountInfo(governanceConfig);
  if (!after) throw new Error("config vanished after update");
  const now = decode(after.data);
  console.log(
    `  forfeit_destination after  ${now.forfeitDestination.toBase58()}`
  );
  console.log(`  quorum_bps                 ${now.quorumBps}`);
  console.log(`  deposit_amount             ${now.depositAmount}`);
  console.log(`  vote_lock_secs             ${now.voteLockSecs}`);

  const unchanged =
    now.admin.equals(prev.admin) &&
    now.mint.equals(prev.mint) &&
    now.totalOpenSupply === prev.totalOpenSupply &&
    now.quorumBps === prev.quorumBps &&
    now.thresholdSimpleBps === prev.thresholdSimpleBps &&
    now.thresholdTreasuryBps === prev.thresholdTreasuryBps &&
    now.thresholdUpgradeBps === prev.thresholdUpgradeBps &&
    now.quorumUpgradeBps === prev.quorumUpgradeBps &&
    now.depositAmount === prev.depositAmount &&
    now.voteLockSecs === prev.voteLockSecs;
  if (!unchanged) {
    throw new Error(
      "a policy field moved; expected only the address to change"
    );
  }
  if (!now.forfeitDestination.equals(FORFEIT_DESTINATION)) {
    throw new Error("forfeit_destination did not take");
  }
  console.log(
    "  verified: destination corrected, every policy field unchanged"
  );
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
