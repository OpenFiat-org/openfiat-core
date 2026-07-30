/**
 * Stakes a devnet node's own identity wallet as a `NodeOperator`.
 *
 * # Why a node needs this at all
 *
 * Stake-weighted QoS (OFS-1600) and the connectivity-tiered reward split
 * both read a node's `StakeAccount`. A node with no stake is a real peer
 * that gossips and serves RPC, but it is invisible to every mechanism that
 * ranks or pays nodes — so a devnet cluster with unstaked nodes cannot
 * exercise those paths at all.
 *
 * # The prerequisite that is easy to miss
 *
 * `openfiat-node` calls `load_or_generate_wallet`, which falls back to
 * `Wallet::generate()` when `CLI_WALLET_PATH` holds no usable keyfile — a
 * **fresh identity on every boot**. Staking to such a node is worse than
 * useless: the stake is stranded against a pubkey that ceases to exist at
 * the next restart, and the OPEN cannot be recovered without the (now lost)
 * key. Confirm the node logs `loaded node identity from <path>` and NOT
 * `generating a fresh identity for this run` before running this.
 *
 * Usage:
 *   NODE_WALLET=/path/to/node/wallet.json OPEN_AMOUNT=100000 \
 *     npx ts-node scripts/stake-node-operator.ts [--commit]
 *
 * The funding wallet (SOL for rent/fees, and the OPEN itself) is the
 * default Solana keypair unless OPEN_SOURCE_KEYPAIR names another.
 */
import * as fs from "fs";
import * as path from "path";
import * as crypto from "crypto";
import {
  Connection,
  Keypair,
  PublicKey,
  SystemProgram,
  Transaction,
  TransactionInstruction,
  clusterApiUrl,
} from "@solana/web3.js";

const STAKING = new PublicKey("HYEXk8XQukBkZbiYB33JyVefQDxqyCpPudad3wBCyYmx");
const GOVERNANCE = new PublicKey("AVJfKUjHsizkGGUy8sdz4Xma2hVgmgvgg8GmUMs8E4eE");
const OPEN_MINT = new PublicKey("29w8TroBTYoaqrXBDcpv5L54VZRA8Kf7kU5U1cakvFdj");
const TOKEN_2022 = new PublicKey("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");
const ATA_PROGRAM = new PublicKey("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");

/** `Role::NodeOperator` — third variant, and part of the StakeAccount seeds,
 *  so a wrong value silently derives a different account rather than failing. */
const ROLE_NODE_OPERATOR = 2;

/** OPEN has 9 decimals. */
const OPEN = (whole: bigint) => whole * 1_000_000_000n;

/** SOL sent to the node wallet so it can pay its own rent and fees:
 *  StakeAccount rent (~0.0015) + an OPEN ATA (~0.0021) + signatures. */
const SOL_FOR_NODE = 0.05;

const disc = (name: string) =>
  crypto.createHash("sha256").update(`global:${name}`).digest().subarray(0, 8);

const u64 = (v: bigint) => {
  const b = Buffer.alloc(8);
  b.writeBigUInt64LE(v);
  return b;
};

function loadKeypair(p: string): Keypair {
  return Keypair.fromSecretKey(
    Uint8Array.from(JSON.parse(fs.readFileSync(p, "utf-8"))),
  );
}

const ata = (owner: PublicKey, mint: PublicKey) =>
  PublicKey.findProgramAddressSync(
    [owner.toBuffer(), TOKEN_2022.toBuffer(), mint.toBuffer()],
    ATA_PROGRAM,
  )[0];

async function send(
  connection: Connection,
  ixs: TransactionInstruction[],
  signers: Keypair[],
  label: string,
  commit: boolean,
): Promise<void> {
  const { blockhash } = await connection.getLatestBlockhash();
  const tx = new Transaction({
    feePayer: signers[0]!.publicKey,
    recentBlockhash: blockhash,
  }).add(...ixs);

  const sim = await connection.simulateTransaction(tx);
  if (sim.value.err) {
    console.error(`  ${label}: simulation failed`, sim.value.err);
    console.error((sim.value.logs || []).join("\n"));
    process.exit(1);
  }
  console.log(`  ${label}: simulation ok`);
  if (!commit) return;

  tx.sign(...signers);
  const sig = await connection.sendRawTransaction(tx.serialize());
  await connection.confirmTransaction(sig, "confirmed");
  console.log(`  ${label}: ${sig}`);
}

async function main() {
  const commit = process.argv.includes("--commit");
  const connection = new Connection(
    process.env.SOLANA_RPC_URL || clusterApiUrl("devnet"),
    "confirmed",
  );

  const nodeWalletPath = process.env.NODE_WALLET;
  if (!nodeWalletPath) throw new Error("set NODE_WALLET to the node's wallet.json");
  const node = loadKeypair(nodeWalletPath);

  const funder = loadKeypair(
    process.env.OPEN_SOURCE_KEYPAIR ||
      path.join(process.env.HOME || "~", ".config/solana/id.json"),
  );

  const amount = OPEN(BigInt(process.env.OPEN_AMOUNT || "100000"));

  const [stakingConfig] = PublicKey.findProgramAddressSync(
    [Buffer.from("staking_config")],
    STAKING,
  );
  const [stakeVault] = PublicKey.findProgramAddressSync(
    [Buffer.from("stake_vault")],
    STAKING,
  );
  const [stakeAccount] = PublicKey.findProgramAddressSync(
    [Buffer.from("stake"), node.publicKey.toBuffer(), Buffer.from([ROLE_NODE_OPERATOR])],
    STAKING,
  );
  const [banRecord] = PublicKey.findProgramAddressSync(
    [Buffer.from("ban"), node.publicKey.toBuffer()],
    GOVERNANCE,
  );
  const nodeOpen = ata(node.publicKey, OPEN_MINT);
  const funderOpen = ata(funder.publicKey, OPEN_MINT);

  console.log("node identity :", node.publicKey.toBase58());
  console.log("stake account :", stakeAccount.toBase58());
  console.log("amount        :", (amount / 1_000_000_000n).toString(), "OPEN");

  const funderBal = await connection.getTokenAccountBalance(funderOpen).catch(() => null);
  console.log("funder OPEN   :", funderBal?.value.uiAmountString ?? "0");
  if (!funderBal || BigInt(funderBal.value.amount) < amount) {
    throw new Error(
      `funding wallet holds ${funderBal?.value.uiAmountString ?? 0} OPEN, needs ` +
        `${(amount / 1_000_000_000n).toString()} — OPEN cannot be minted (mint ` +
        `authority is permanently unset), so top up from an existing stash`,
    );
  }

  // 1. SOL for the node's own rent and fees.
  const nodeSol = await connection.getBalance(node.publicKey);
  if (nodeSol < SOL_FOR_NODE * 1e9 * 0.5) {
    await send(
      connection,
      [
        SystemProgram.transfer({
          fromPubkey: funder.publicKey,
          toPubkey: node.publicKey,
          lamports: Math.round(SOL_FOR_NODE * 1e9),
        }),
      ],
      [funder],
      "fund node SOL",
      commit,
    );
  } else {
    console.log("  fund node SOL: already funded, skipping");
  }

  // 2. OPEN into the node's own ATA, creating it if needed.
  const nodeOpenInfo = await connection.getAccountInfo(nodeOpen);
  const transferIxs: TransactionInstruction[] = [];
  if (!nodeOpenInfo) {
    transferIxs.push(
      new TransactionInstruction({
        programId: ATA_PROGRAM,
        keys: [
          { pubkey: funder.publicKey, isSigner: true, isWritable: true },
          { pubkey: nodeOpen, isSigner: false, isWritable: true },
          { pubkey: node.publicKey, isSigner: false, isWritable: false },
          { pubkey: OPEN_MINT, isSigner: false, isWritable: false },
          { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
          { pubkey: TOKEN_2022, isSigner: false, isWritable: false },
        ],
        data: Buffer.from([1]), // CreateIdempotent
      }),
    );
  }
  // TransferChecked (Token-2022 instruction 12): amount + decimals.
  transferIxs.push(
    new TransactionInstruction({
      programId: TOKEN_2022,
      keys: [
        { pubkey: funderOpen, isSigner: false, isWritable: true },
        { pubkey: OPEN_MINT, isSigner: false, isWritable: false },
        { pubkey: nodeOpen, isSigner: false, isWritable: true },
        { pubkey: funder.publicKey, isSigner: true, isWritable: false },
      ],
      data: Buffer.concat([Buffer.from([12]), u64(amount), Buffer.from([9])]),
    }),
  );
  await send(connection, transferIxs, [funder], "transfer OPEN to node", commit);

  // 3. StakeAccount, if this role has never been initialized for this wallet.
  const existing = await connection.getAccountInfo(stakeAccount);
  if (!existing) {
    await send(
      connection,
      [
        new TransactionInstruction({
          programId: STAKING,
          keys: [
            { pubkey: node.publicKey, isSigner: true, isWritable: true },
            { pubkey: stakeAccount, isSigner: false, isWritable: true },
            { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
          ],
          data: Buffer.concat([
            disc("initialize_stake_account"),
            Buffer.from([ROLE_NODE_OPERATOR]),
          ]),
        }),
      ],
      [node],
      "initialize stake account",
      commit,
    );
  } else {
    console.log("  initialize stake account: already exists, skipping");
  }

  // 4. Stake.
  await send(
    connection,
    [
      new TransactionInstruction({
        programId: STAKING,
        keys: [
          { pubkey: node.publicKey, isSigner: true, isWritable: true },
          { pubkey: banRecord, isSigner: false, isWritable: false },
          { pubkey: stakingConfig, isSigner: false, isWritable: false },
          { pubkey: stakeAccount, isSigner: false, isWritable: true },
          { pubkey: stakeVault, isSigner: false, isWritable: true },
          { pubkey: nodeOpen, isSigner: false, isWritable: true },
          { pubkey: OPEN_MINT, isSigner: false, isWritable: false },
          { pubkey: TOKEN_2022, isSigner: false, isWritable: false },
        ],
        data: Buffer.concat([disc("stake"), u64(amount)]),
      }),
    ],
    [node],
    "stake",
    commit,
  );

  if (!commit) {
    console.log("\ndry run — nothing sent. Re-run with --commit to apply.");
    return;
  }

  // Read the staked amount back off chain rather than trusting the sends.
  const after = await connection.getAccountInfo(stakeAccount);
  if (!after) throw new Error("stake account missing after a confirmed stake");
  // disc(8) owner(32) role(1) amount(8)
  const staked = after.data.readBigUInt64LE(41);
  console.log("\nverified on chain:");
  console.log("  stake account:", stakeAccount.toBase58(), `(${after.data.length} bytes)`);
  console.log("  role         :", after.data[40]);
  console.log("  staked       :", (staked / 1_000_000_000n).toString(), "OPEN");
  if (staked < amount) {
    throw new Error(`staked ${staked} is less than the requested ${amount}`);
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
