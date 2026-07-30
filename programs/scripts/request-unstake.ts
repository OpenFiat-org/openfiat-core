/**
 * Starts unbonding for a node that is being retired.
 *
 * # Why this cannot be "recover the OPEN before shutting down"
 *
 * Staked OPEN is not withdrawable on demand. `request_unstake` moves the
 * amount into `unbonding_amount` and stamps `unbonding_release_at`; only
 * after the configured unbonding period (604,800s — seven days on this
 * cluster) does `withdraw_unstaked` return it. That delay is the whole
 * point of a bond: stake that could be pulled the instant a dispute went
 * badly would not be at risk, and so would not secure anything.
 *
 * So retiring a node is a two-step operation days apart. What matters at
 * shutdown time is not the tokens — it is the KEY. The stake is bound to
 * the wallet, not to the machine or the container, so as long as the
 * keypair survives the OPEN is recoverable at any later date. Deleting a
 * node's data directory without its keypair backed up elsewhere is what
 * actually destroys value here.
 *
 * Usage:
 *   NODE_WALLET=/path/wallet.json npx ts-node scripts/request-unstake.ts [--commit]
 */
import * as fs from "fs";
import * as crypto from "crypto";
import {
  Connection,
  Keypair,
  PublicKey,
  Transaction,
  TransactionInstruction,
  clusterApiUrl,
} from "@solana/web3.js";

const STAKING = new PublicKey("HYEXk8XQukBkZbiYB33JyVefQDxqyCpPudad3wBCyYmx");
/** `Role::NodeOperator` — part of the StakeAccount seeds. */
const ROLE_NODE_OPERATOR = 2;

const disc = (name: string) =>
  crypto.createHash("sha256").update(`global:${name}`).digest().subarray(0, 8);

const u64 = (v: bigint) => {
  const b = Buffer.alloc(8);
  b.writeBigUInt64LE(v);
  return b;
};

async function main() {
  const commit = process.argv.includes("--commit");
  const connection = new Connection(
    process.env.SOLANA_RPC_URL || clusterApiUrl("devnet"),
    "confirmed",
  );

  const path = process.env.NODE_WALLET;
  if (!path) throw new Error("set NODE_WALLET to the node's wallet.json");
  const owner = Keypair.fromSecretKey(
    Uint8Array.from(JSON.parse(fs.readFileSync(path, "utf-8"))),
  );

  const [stakingConfig] = PublicKey.findProgramAddressSync(
    [Buffer.from("staking_config")],
    STAKING,
  );
  const [stakeAccount] = PublicKey.findProgramAddressSync(
    [Buffer.from("stake"), owner.publicKey.toBuffer(), Buffer.from([ROLE_NODE_OPERATOR])],
    STAKING,
  );

  const before = await connection.getAccountInfo(stakeAccount);
  if (!before) throw new Error(`no stake account at ${stakeAccount.toBase58()}`);
  // disc(8) owner(32) role(1) amount(8) unbonding_amount(8) unbonding_release_at(8)
  const staked = before.data.readBigUInt64LE(41);
  const alreadyUnbonding = before.data.readBigUInt64LE(49);

  console.log("owner        :", owner.publicKey.toBase58());
  console.log("stake account:", stakeAccount.toBase58());
  console.log("staked       :", (staked / 1_000_000_000n).toString(), "OPEN");
  console.log("unbonding    :", (alreadyUnbonding / 1_000_000_000n).toString(), "OPEN");

  if (staked === 0n) {
    console.log("nothing staked — nothing to unbond.");
    return;
  }

  const ix = new TransactionInstruction({
    programId: STAKING,
    keys: [
      { pubkey: owner.publicKey, isSigner: true, isWritable: true },
      { pubkey: stakingConfig, isSigner: false, isWritable: false },
      { pubkey: stakeAccount, isSigner: false, isWritable: true },
    ],
    // The full staked amount: this node is being retired, so a partial
    // unbond would leave a remainder needing a second seven-day wait.
    data: Buffer.concat([disc("request_unstake"), u64(staked)]),
  });

  const { blockhash } = await connection.getLatestBlockhash();
  const tx = new Transaction({
    feePayer: owner.publicKey,
    recentBlockhash: blockhash,
  }).add(ix);

  const sim = await connection.simulateTransaction(tx);
  if (sim.value.err) {
    console.error("simulation failed:", sim.value.err);
    console.error((sim.value.logs || []).join("\n"));
    process.exit(1);
  }
  console.log("simulation: ok");
  if (!commit) {
    console.log("\ndry run — nothing sent. Re-run with --commit to apply.");
    return;
  }

  tx.sign(owner);
  const signature = await connection.sendRawTransaction(tx.serialize());
  await connection.confirmTransaction(signature, "confirmed");
  console.log("signature:", signature);

  // Read back rather than trust the send: the release timestamp is the
  // thing an operator has to plan around, so it must come off chain.
  const after = await connection.getAccountInfo(stakeAccount);
  const nowStaked = after!.data.readBigUInt64LE(41);
  const unbonding = after!.data.readBigUInt64LE(49);
  const releaseAt = after!.data.readBigInt64LE(57);
  console.log("verified on chain:");
  console.log("  staked now :", (nowStaked / 1_000_000_000n).toString(), "OPEN");
  console.log("  unbonding  :", (unbonding / 1_000_000_000n).toString(), "OPEN");
  console.log("  withdrawable after:", new Date(Number(releaseAt) * 1000).toISOString());
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
