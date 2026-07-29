/**
 * Lowers devnet's `GovernanceConfig.vote_lock_secs` so governance is
 * actually exercisable there.
 *
 * Devnet was carrying 604 800 seconds — seven days. That is a defensible
 * production value and an unusable test one: once listing and delisting a
 * wallet require a passed proposal, every governance action on devnet
 * needs a week between the vote closing and the action landing. Nothing
 * about the proposal-gated ban path could be demonstrated.
 *
 * Run BEFORE upgrading the governance program, not after. This calls
 * `update_governance_config`, which exists in the currently-deployed
 * binary and is proven working; sequencing it after the upgrade would
 * mean a fix step depending on freshly-deployed code. Earlier in this
 * project a devnet fix script was written to call an instruction that
 * only existed in the not-yet-deployed binary, and the ordering had to be
 * inverted.
 *
 * This uses `admin`'s remaining authority over `vote_lock_secs`. That
 * authority is bounded by `MAX_VOTE_LOCK_SECS` but not removed, and it is
 * a real residual power: parked at its ceiling it would leave every
 * accepted proposal — including one delisting a wallet — unexecutable for
 * a month. Using it to shorten the lock on a test network is the benign
 * direction. On mainnet, 60 seconds would not be.
 *
 *   npx tsx scripts/set-devnet-vote-lock.ts --dry-run
 *   npx tsx scripts/set-devnet-vote-lock.ts
 */

import { readFileSync } from "node:fs";
import { homedir } from "node:os";

import {
  Connection,
  Keypair,
  PublicKey,
  Transaction,
  TransactionInstruction,
} from "@solana/web3.js";

const RPC_URL = process.env.SOLANA_RPC_URL ?? "https://api.devnet.solana.com";
const GOVERNANCE_PROGRAM_ID = new PublicKey(
  "AVJfKUjHsizkGGUy8sdz4Xma2hVgmgvgg8GmUMs8E4eE"
);
const MINT = new PublicKey("29w8TroBTYoaqrXBDcpv5L54VZRA8Kf7kU5U1cakvFdj");

/** Long enough that the timelock is still a real gate a test must wait
 *  out, short enough to wait out inside one run. */
const TARGET_VOTE_LOCK_SECS = 60n;

/** From `target/idl/governance.json`, not hand-computed. */
const DISCRIMINATOR = Buffer.from([140, 45, 181, 17, 77, 67, 157, 248]);

/**
 * Walks `GovernanceConfig` with a single moving cursor and asserts the
 * byte count, rather than reading fields at independently-computed
 * offsets (OFS-4200 §7.1).
 *
 * A per-field offset that omits a field shifts every field after it and
 * still yields well-formed pubkeys and plausible integers. That happened
 * in this repo: a `StakingConfig` decode skipped a 32-byte field and
 * reported two adjacent pubkeys with their identities swapped. It read as
 * correct and was published before the chain contradicted it. A cursor
 * cannot skip a field without failing the balance check.
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
  console.log(`  account size          ${before.data.length} bytes`);
  console.log(`  vote_lock_secs before ${prev.voteLockSecs}`);
  console.log(`  vote_lock_secs target ${TARGET_VOTE_LOCK_SECS}`);

  if (!prev.admin.equals(admin.publicKey)) {
    throw new Error(
      `loaded wallet ${admin.publicKey.toBase58()} is not the config admin ` +
        `${prev.admin.toBase58()}`
    );
  }

  if (process.argv.includes("--dry-run")) {
    console.log("  dry run — decoded cleanly, no transaction sent");
    console.table(
      Object.fromEntries(Object.entries(prev).map(([k, v]) => [k, String(v)]))
    );
    return;
  }

  if (prev.voteLockSecs === TARGET_VOTE_LOCK_SECS) {
    console.log("  already at the target — nothing to do");
    return;
  }

  // Every field is echoed back from the decode rather than restated as a
  // constant, including `forfeit_destination`. This instruction rewrites
  // the whole config, so a hardcoded value here would silently revert
  // whatever the live account actually holds.
  const data = Buffer.concat([
    DISCRIMINATOR,
    u64LE(prev.totalOpenSupply),
    u16LE(prev.quorumBps),
    u16LE(prev.thresholdSimpleBps),
    u16LE(prev.thresholdTreasuryBps),
    u16LE(prev.thresholdUpgradeBps),
    u16LE(prev.quorumUpgradeBps),
    u64LE(prev.depositAmount),
    i64LE(TARGET_VOTE_LOCK_SECS),
  ]);

  const ix = new TransactionInstruction({
    programId: GOVERNANCE_PROGRAM_ID,
    keys: [
      { pubkey: admin.publicKey, isSigner: true, isWritable: false },
      { pubkey: governanceConfig, isSigner: false, isWritable: true },
      { pubkey: MINT, isSigner: false, isWritable: false },
      { pubkey: prev.forfeitDestination, isSigner: false, isWritable: false },
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

  // Read the account back rather than trusting that the transaction
  // succeeding means the field took the value intended.
  const after = await connection.getAccountInfo(governanceConfig);
  if (!after) throw new Error("config vanished after update");
  const now = decode(after.data);
  console.log(`  vote_lock_secs after  ${now.voteLockSecs}`);

  if (now.voteLockSecs !== TARGET_VOTE_LOCK_SECS) {
    throw new Error(
      `vote_lock_secs is ${now.voteLockSecs}, expected ${TARGET_VOTE_LOCK_SECS}`
    );
  }

  const unchanged =
    now.admin.equals(prev.admin) &&
    now.mint.equals(prev.mint) &&
    now.forfeitDestination.equals(prev.forfeitDestination) &&
    now.totalOpenSupply === prev.totalOpenSupply &&
    now.quorumBps === prev.quorumBps &&
    now.thresholdSimpleBps === prev.thresholdSimpleBps &&
    now.thresholdTreasuryBps === prev.thresholdTreasuryBps &&
    now.thresholdUpgradeBps === prev.thresholdUpgradeBps &&
    now.quorumUpgradeBps === prev.quorumUpgradeBps &&
    now.depositAmount === prev.depositAmount;
  if (!unchanged) {
    throw new Error(
      "a field other than vote_lock_secs moved; this instruction rewrites " +
        "the whole config, so that means a value was not echoed back correctly"
    );
  }

  console.log("  every other field unchanged");
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
