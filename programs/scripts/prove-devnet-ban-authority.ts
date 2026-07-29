/**
 * Proves on deployed devnet that the single-key ban authority is gone.
 *
 * Supersedes `prove-devnet-ban-list.ts`, which proved the old behaviour:
 * that `admin` could list a wallet and the deployed staking program would
 * then refuse its deposit. `admin` can no longer list anything, so that
 * script's central action is exactly what must now fail.
 *
 * # What this can and cannot prove, and why
 *
 * The positive path — pass a proposal, execute it, watch a wallet get
 * listed and then delisted — is NOT provable on devnet, and no amount of
 * scripting fixes that:
 *
 *   - `create_proposal` transfers `GovernanceConfig.deposit_amount`
 *     (5 000 OPEN) into the deposit vault.
 *   - `cast_vote` weighs a real `StakeAccount`, and quorum is 10% of
 *     `total_open_supply` — 100 000 000 OPEN.
 *   - The devnet OPEN mint's authority is permanently unset, so no more
 *     OPEN can ever be minted, and the wallets holding the genesis supply
 *     have unrecoverable keypairs.
 *
 * So nobody on devnet can fund a proposal or reach quorum. That is a
 * pre-existing property of this deployment, not something the governance
 * change introduced. The positive path is proven instead by the 92-test
 * `anchor test --validator legacy` suite against a real validator, where
 * a controllable mint exists.
 *
 * What IS provable here is the security property that was actually asked
 * for: the key that could previously deny any wallet deposit access
 * protocol-wide can no longer do so. That is a negative, and a negative
 * proven against the deployed binary is worth more than a positive proven
 * only in tests.
 *
 * A consequence worth stating plainly: with no reachable OPEN, the devnet
 * ban list is now inert in BOTH directions — nothing can be listed, and
 * nothing could be delisted either. Inert is the safe failure. One key
 * able to exclude anyone was the unsafe one. No `BanRecord` exists on
 * devnet, so nothing is stranded by this.
 *
 *   npx tsx scripts/prove-devnet-ban-authority.ts
 */

import { readFileSync } from "node:fs";
import { homedir } from "node:os";

import {
  Connection,
  Keypair,
  PublicKey,
  SystemProgram,
  Transaction,
  TransactionInstruction,
} from "@solana/web3.js";

const RPC_URL = process.env.SOLANA_RPC_URL ?? "https://api.devnet.solana.com";
const GOVERNANCE_PROGRAM_ID = new PublicKey(
  "AVJfKUjHsizkGGUy8sdz4Xma2hVgmgvgg8GmUMs8E4eE"
);

/** From `target/idl/governance.json`, not hand-computed. */
const LIST_WALLET_DISCRIMINATOR = Buffer.from([
  176, 149, 148, 11, 126, 182, 162, 248,
]);

function assert(cond: unknown, msg: string): asserts cond {
  if (!cond) throw new Error(`ASSERTION FAILED: ${msg}`);
}

/** Walks `GovernanceConfig` with one moving cursor and asserts the byte
 *  count (OFS-4200 §7.1). Per-field offsets can skip a field and still
 *  produce well-formed pubkeys; a cursor cannot. */
function decodeGovernanceConfig(data: Buffer) {
  let o = 8;
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

  const TRAILING_BUMPS = 2;
  if (o !== data.length - TRAILING_BUMPS) {
    throw new Error(
      `decode walked ${o} bytes, expected ${data.length - TRAILING_BUMPS} — ` +
        `the field walk does not match the on-chain struct`
    );
  }
  return decoded;
}

async function sendExpectingFailure(
  connection: Connection,
  signer: Keypair,
  ix: TransactionInstruction,
  what: string
): Promise<string> {
  const { blockhash, lastValidBlockHeight } =
    await connection.getLatestBlockhash();
  const tx = new Transaction({
    feePayer: signer.publicKey,
    blockhash,
    lastValidBlockHeight,
  }).add(ix);
  tx.sign(signer);
  try {
    const sig = await connection.sendRawTransaction(tx.serialize());
    await connection.confirmTransaction(
      { signature: sig, blockhash, lastValidBlockHeight },
      "confirmed"
    );
    throw new Error(`ASSERTION FAILED: ${what} was ACCEPTED (${sig})`);
  } catch (error: unknown) {
    const text = String(
      (error as { logs?: string[] })?.logs?.join("\n") ?? error
    );
    if (text.includes("ASSERTION FAILED")) throw error;
    return text;
  }
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
  const info = await connection.getAccountInfo(governanceConfig);
  assert(info, `no governance config at ${governanceConfig.toBase58()}`);
  const config = decodeGovernanceConfig(info.data);

  console.log(`governance ${GOVERNANCE_PROGRAM_ID.toBase58()}`);
  console.log(`  config          ${governanceConfig.toBase58()}`);
  console.log(`  admin           ${config.admin.toBase58()}`);
  console.log(`  vote_lock_secs  ${config.voteLockSecs}`);

  // The whole point rests on this: the wallet running the script IS the
  // config admin. A failure below from some unrelated wallet would prove
  // nothing at all.
  assert(
    config.admin.equals(admin.publicKey),
    `this proof requires running as the config admin; loaded ` +
      `${admin.publicKey.toBase58()}, config names ${config.admin.toBase58()}`
  );
  console.log("  running AS the former ban authority\n");

  // ---- 1. the former admin cannot list a wallet ---------------------
  //
  // Not "is not allowed to" — cannot. `list_wallet` requires a proposal
  // account and the `ProposalAction` derived from it, and admin has no
  // way to bring either into existence: creating a proposal costs 5 000
  // OPEN it does not have, and the accounts are PDA-checked so no
  // substitute stands in.
  const victim = Keypair.generate().publicKey;
  const [banRecord] = PublicKey.findProgramAddressSync(
    [Buffer.from("ban"), victim.toBuffer()],
    GOVERNANCE_PROGRAM_ID
  );
  const [absentProposal] = PublicKey.findProgramAddressSync(
    // Proposal id 0 has never been created on this deployment; asserted
    // below rather than assumed.
    [Buffer.from("proposal"), Buffer.alloc(8, 0)],
    GOVERNANCE_PROGRAM_ID
  );
  const [absentAction] = PublicKey.findProgramAddressSync(
    [Buffer.from("proposal_action"), absentProposal.toBuffer()],
    GOVERNANCE_PROGRAM_ID
  );
  assert(
    (await connection.getAccountInfo(absentProposal)) === null,
    "the proposal used for this attempt must not exist for it to be a real attack"
  );

  const listAttempt = new TransactionInstruction({
    programId: GOVERNANCE_PROGRAM_ID,
    keys: [
      { pubkey: admin.publicKey, isSigner: true, isWritable: true },
      { pubkey: governanceConfig, isSigner: false, isWritable: false },
      { pubkey: absentProposal, isSigner: false, isWritable: true },
      { pubkey: absentAction, isSigner: false, isWritable: false },
      { pubkey: banRecord, isSigner: false, isWritable: true },
      { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
    ],
    data: Buffer.concat([LIST_WALLET_DISCRIMINATOR, victim.toBuffer()]),
  });

  const logs = await sendExpectingFailure(
    connection,
    admin,
    listAttempt,
    "admin listing a wallet with no proposal"
  );
  assert(
    /AccountNotInitialized|ConstraintSeeds|AnchorError/.test(logs),
    `expected the missing proposal to be rejected, got:\n${logs}`
  );
  console.log("1. admin CANNOT list a wallet — no proposal, no listing");

  assert(
    (await connection.getAccountInfo(banRecord)) === null,
    "a BanRecord was created despite the instruction failing"
  );
  console.log("   no BanRecord was created\n");

  // ---- 2. nothing is banned, and nothing is stranded ----------------
  //
  // Checked because delisting now needs a passed proposal too. A wallet
  // listed before this upgrade would be unbannable-in-reverse on a
  // network where no proposal can pass — worth knowing about rather than
  // discovering later.
  const banRecords = await connection.getProgramAccounts(
    GOVERNANCE_PROGRAM_ID,
    { filters: [{ dataSize: 114 }] }
  );
  assert(
    banRecords.length === 0,
    `${banRecords.length} BanRecord(s) exist and cannot be delisted on a ` +
      `network where no proposal can reach quorum: ` +
      banRecords.map((a) => a.pubkey.toBase58()).join(", ")
  );
  console.log("2. no wallet is listed, so nothing is stranded by the change\n");

  console.log("DEVNET BAN AUTHORITY ASSERTIONS PASSED");
  console.log(
    "\nThe positive path (pass a proposal, list, delist) is not provable\n" +
      "here: devnet's OPEN mint authority is unset and the genesis supply\n" +
      "is unreachable, so no proposal can be funded or reach quorum. It is\n" +
      "proven by `anchor test --validator legacy` (92 passing) against a\n" +
      "validator with a controllable mint."
  );
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
