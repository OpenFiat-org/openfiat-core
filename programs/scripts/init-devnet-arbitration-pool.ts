/**
 * Creates the singleton arbitration pool token account on devnet.
 *
 * # Why this was missing, and what it breaks
 *
 * The PDA at seeds `[b"arbitration_pool"]` had never been created on devnet.
 * Nothing complained, because the account is only ever read down one path:
 * `create_liquidity_vault`'s OPEN carve-out reads it to answer "is this mint
 * OPEN?" — the escrow program has no `open_mint` field of its own, and the
 * pool's mint is its one unambiguous definition of OPEN.
 *
 * So while every allowlisted settlement mint (wSOL, USDC, USDT) kept working
 * normally, creating a merchant's **OPEN** vault failed with
 * `ArbitrationPoolNotInitialized`. That vault is what `charge_ad_listing_fee`
 * debits and what `open_dispute_case` draws arbitration deposits from, so ad
 * listing and arbitration deposits were both dead while the rest of the
 * system looked healthy. That asymmetry is exactly why it went unnoticed.
 *
 * # Why this runs before the pending program upgrade rather than after
 *
 * It could have been folded into the deploy, but doing it now is strictly
 * safer. `initialize_arbitration_pool` takes `fee_config` as a typed
 * `Account<FeeConfig>`, so Anchor deserializes it against whatever struct the
 * *running* program was built with. Today's on-chain FeeConfig is 203 bytes
 * and today's deployed program expects exactly that, so it works. After the
 * upgrade it would have to wait for `migrate_fee_config` to grow the account
 * to 726 first — a real ordering dependency, and one that fails confusingly
 * if it is got backwards. Running it here removes that ordering hazard.
 *
 * The resulting account stays valid across the upgrade: it is a plain SPL
 * Token-2022 account, and the new program reads it with
 * `TokenAccount::try_deserialize` after checking its owner is one of
 * `TokenInterface::ids()`, which Token-2022 is.
 *
 * Verified against the deployed program by simulation before being written:
 * the instruction exists there (it reached `InitializeAccount3`), while a
 * nonexistent discriminator fell through to Anchor's error 101.
 *
 * Usage: npx ts-node scripts/init-devnet-arbitration-pool.ts [--commit]
 * Without `--commit` it simulates and changes nothing.
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

const ESCROW_PROGRAM_ID = new PublicKey(
  "HaPpM1QYM3dKp3sX7zhEdft9hB6ncu6xfALAbkyQChQP",
);
/** Arbitration deposits are OPEN-denominated (OFS-4100 §6). */
const OPEN_MINT = new PublicKey("29w8TroBTYoaqrXBDcpv5L54VZRA8Kf7kU5U1cakvFdj");
const TOKEN_2022_PROGRAM_ID = new PublicKey(
  "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb",
);

const discriminator = (name: string) =>
  crypto.createHash("sha256").update(`global:${name}`).digest().subarray(0, 8);

/** SPL token account layout: mint(32) owner(32) ... — enough to verify. */
const TOKEN_ACCOUNT_MINT_OFFSET = 0;
const TOKEN_ACCOUNT_OWNER_OFFSET = 32;

async function main() {
  const commit = process.argv.includes("--commit");
  const connection = new Connection(
    process.env.SOLANA_RPC_URL || clusterApiUrl("devnet"),
    "confirmed",
  );
  const keypairPath =
    process.env.SOLANA_KEYPAIR ||
    path.join(process.env.HOME || "~", ".config/solana/id.json");
  const admin = Keypair.fromSecretKey(
    Uint8Array.from(JSON.parse(fs.readFileSync(keypairPath, "utf-8"))),
  );

  const [feeConfig] = PublicKey.findProgramAddressSync(
    [Buffer.from("fee_config")],
    ESCROW_PROGRAM_ID,
  );
  const [arbitrationPool] = PublicKey.findProgramAddressSync(
    [Buffer.from("arbitration_pool")],
    ESCROW_PROGRAM_ID,
  );

  const feeConfigAccount = await connection.getAccountInfo(feeConfig);
  if (!feeConfigAccount) {
    throw new Error(
      `no FeeConfig at ${feeConfig.toBase58()} — run initialize_fee_config first`,
    );
  }
  // The instruction is admin-gated on `fee_config.admin`, which sits right
  // after the 8-byte discriminator. Checked here so an unauthorized key
  // fails with a clear message instead of an opaque on-chain Unauthorized.
  const feeConfigAdmin = new PublicKey(feeConfigAccount.data.subarray(8, 40));
  if (!feeConfigAdmin.equals(admin.publicKey)) {
    throw new Error(
      `FeeConfig admin is ${feeConfigAdmin.toBase58()} but the loaded keypair ` +
        `is ${admin.publicKey.toBase58()}`,
    );
  }

  const existing = await connection.getAccountInfo(arbitrationPool);
  if (existing) {
    const mint = new PublicKey(
      existing.data.subarray(
        TOKEN_ACCOUNT_MINT_OFFSET,
        TOKEN_ACCOUNT_MINT_OFFSET + 32,
      ),
    );
    console.log(
      `arbitration pool already exists at ${arbitrationPool.toBase58()}\n` +
        `  mint: ${mint.toBase58()}`,
    );
    // A singleton cannot be re-initialized, so this is the terminal state
    // either way — but if it holds the wrong mint that is unrecoverable and
    // must be shouted about rather than reported as success.
    if (!mint.equals(OPEN_MINT)) {
      throw new Error(
        `pool holds ${mint.toBase58()}, not OPEN ${OPEN_MINT.toBase58()} — ` +
          `this is permanent; the carve-out in create_liquidity_vault will ` +
          `treat that mint as OPEN`,
      );
    }
    return;
  }

  console.log("escrow      ", ESCROW_PROGRAM_ID.toBase58());
  console.log("fee_config  ", feeConfig.toBase58(), `(${feeConfigAccount.data.length} bytes)`);
  console.log("pool PDA    ", arbitrationPool.toBase58(), "(does not exist yet)");
  console.log("mint        ", OPEN_MINT.toBase58(), "(OPEN, Token-2022)");

  const ix = new TransactionInstruction({
    programId: ESCROW_PROGRAM_ID,
    keys: [
      { pubkey: admin.publicKey, isSigner: true, isWritable: true },
      { pubkey: feeConfig, isSigner: false, isWritable: false },
      { pubkey: OPEN_MINT, isSigner: false, isWritable: false },
      { pubkey: arbitrationPool, isSigner: false, isWritable: true },
      { pubkey: TOKEN_2022_PROGRAM_ID, isSigner: false, isWritable: false },
      { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
    ],
    data: discriminator("initialize_arbitration_pool"),
  });

  const { blockhash } = await connection.getLatestBlockhash();
  const tx = new Transaction({
    feePayer: admin.publicKey,
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

  tx.sign(admin);
  const signature = await connection.sendRawTransaction(tx.serialize());
  await connection.confirmTransaction(signature, "confirmed");
  console.log("signature:", signature);

  // Read the account back rather than trusting the send. The mint is the
  // whole point: it is what the OPEN carve-out compares against, and it
  // cannot be changed afterwards.
  const created = await connection.getAccountInfo(arbitrationPool);
  if (!created) throw new Error("pool still absent after a confirmed transaction");
  const mint = new PublicKey(
    created.data.subarray(TOKEN_ACCOUNT_MINT_OFFSET, TOKEN_ACCOUNT_MINT_OFFSET + 32),
  );
  const owner = new PublicKey(
    created.data.subarray(TOKEN_ACCOUNT_OWNER_OFFSET, TOKEN_ACCOUNT_OWNER_OFFSET + 32),
  );
  console.log("verified on chain:");
  console.log("  owner program:", created.owner.toBase58());
  console.log("  mint:         ", mint.toBase58());
  console.log("  authority:    ", owner.toBase58());

  if (!mint.equals(OPEN_MINT)) throw new Error("pool mint is not OPEN");
  if (!owner.equals(feeConfig)) {
    throw new Error(
      `pool authority is ${owner.toBase58()}, expected the FeeConfig PDA ` +
        `${feeConfig.toBase58()} — only the program may move what the pool holds`,
    );
  }
  if (!created.owner.equals(TOKEN_2022_PROGRAM_ID)) {
    throw new Error(`pool is owned by ${created.owner.toBase58()}, not Token-2022`);
  }
  console.log("\npool is correct: OPEN-denominated, program-controlled.");
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
