/**
 * One-shot devnet fix for the `FeeConfig` treasury addresses.
 *
 * `initialize_fee_config` was originally called with the treasury *owner*
 * wallets rather than their token accounts. `release_escrow` requires each
 * treasury to deserialize as a Token-2022 `TokenAccount`, so the stored
 * wallet addresses made every settlement — and every `BuyerWins` dispute —
 * impossible to execute. The four addresses did not exist on chain at all.
 *
 * This creates the associated token account for each owner against the devnet
 * settlement (USDC) mint, then points `FeeConfig` at those ATAs via the program's
 * own `update_fee_config`, whose account constraints make storing a
 * non-token-account impossible in future.
 *
 *   npx ts-node scripts/init-devnet-fee-treasuries.ts
 *
 * Safe to re-run: ATA creation is skipped when the account already exists,
 * and `update_fee_config` simply rewrites the same values.
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
  getAssociatedTokenAddressSync,
  createAssociatedTokenAccountInstruction,
} from "@solana/spl-token";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { homedir } from "node:os";

const RPC_URL = process.env.SOLANA_RPC_URL ?? "https://api.devnet.solana.com";
const ESCROW_PROGRAM_ID = new PublicKey(
  "HaPpM1QYM3dKp3sX7zhEdft9hB6ncu6xfALAbkyQChQP",
);
/**
 * The settlement fee is a slice of the *traded* amount, and `release_escrow`
 * moves it with `transfer_checked` from the trade's own escrow vault. The
 * treasuries must therefore hold the mint trades settle in — the stablecoin,
 * not OPEN. Pointing them at OPEN would leave every USDC trade failing on a
 * mint mismatch: the same defect this script exists to fix, one layer down.
 *
 * Corollary worth recording: one `FeeConfig` carries exactly one treasury
 * set, so fee collection currently works for a single settlement mint. A
 * second traded stablecoin needs per-mint treasuries.
 */
const SETTLEMENT_MINT = new PublicKey(
  process.env.SETTLEMENT_MINT ?? "SK1JEbfsjjTG2WELNirmM7iJVcdnwerqfF32kCnoWsM",
);

/** Treasury *owners* — the wallets that were wrongly stored as treasuries. */
const OWNERS = {
  dev: new PublicKey("HLEB5akyStXEZfsTtgpnzexC4gyvDS5QNiYm1vDSHX4p"),
  ecosystem: new PublicKey("VueuQemTWZcZXMhfN1jRH1vs1zeWXYvzhDPC8uxJARF"),
  infra: new PublicKey("Aj2ciiPQJcun6fMcr1WCmcbKrMkyRsKrbFeDf2KKqySZ"),
  emergency: new PublicKey("836SJH2LfCgzV4rseMgGSTTPTTM3mMjkXp2exgkry6SH"),
};

/** OFS-4100 §5: 15 bps settlement fee, split dev/eco/infra/emergency 40/30/20/10. */
const PARAMS = {
  adListingFee: 0n,
  disputeFilingFee: 0n,
  settlementFeeBps: 15,
  devTreasuryBps: 4000,
  ecosystemTreasuryBps: 3000,
  infraTreasuryBps: 2000,
  emergencyReserveBps: 1000,
  timeoutSecs: 1800n,
};

function anchorDiscriminator(name: string): Buffer {
  return createHash("sha256").update(`global:${name}`).digest().subarray(0, 8);
}

function encodeParams(): Buffer {
  const b = Buffer.alloc(8 + 8 + 2 + 2 + 2 + 2 + 2 + 8);
  let o = 0;
  b.writeBigUInt64LE(PARAMS.adListingFee, o); o += 8;
  b.writeBigUInt64LE(PARAMS.disputeFilingFee, o); o += 8;
  b.writeUInt16LE(PARAMS.settlementFeeBps, o); o += 2;
  b.writeUInt16LE(PARAMS.devTreasuryBps, o); o += 2;
  b.writeUInt16LE(PARAMS.ecosystemTreasuryBps, o); o += 2;
  b.writeUInt16LE(PARAMS.infraTreasuryBps, o); o += 2;
  b.writeUInt16LE(PARAMS.emergencyReserveBps, o); o += 2;
  b.writeBigInt64LE(PARAMS.timeoutSecs, o);
  return b;
}

async function main() {
  const connection = new Connection(RPC_URL, "confirmed");
  const admin = Keypair.fromSecretKey(
    Uint8Array.from(
      JSON.parse(
        readFileSync(`${homedir()}/.config/solana/id.json`, "utf8"),
      ) as number[],
    ),
  );
  console.log("admin:", admin.publicKey.toBase58());

  const [feeConfig] = PublicKey.findProgramAddressSync(
    [Buffer.from("fee_config")],
    ESCROW_PROGRAM_ID,
  );

  // 1. Create each treasury ATA if it isn't there yet.
  const atas: Record<string, PublicKey> = {};
  const createIxs: TransactionInstruction[] = [];
  for (const [name, owner] of Object.entries(OWNERS)) {
    const ata = getAssociatedTokenAddressSync(
      SETTLEMENT_MINT,
      owner,
      true, // owners are plain wallets, but allow off-curve for safety
      TOKEN_2022_PROGRAM_ID,
    );
    atas[name] = ata;
    const info = await connection.getAccountInfo(ata);
    if (info) {
      console.log(`${name} ATA already exists: ${ata.toBase58()}`);
    } else {
      console.log(`${name} ATA to create:      ${ata.toBase58()}`);
      createIxs.push(
        createAssociatedTokenAccountInstruction(
          admin.publicKey,
          ata,
          owner,
          SETTLEMENT_MINT,
          TOKEN_2022_PROGRAM_ID,
        ),
      );
    }
  }

  if (createIxs.length > 0) {
    const tx = new Transaction().add(...createIxs);
    const sig = await connection.sendTransaction(tx, [admin]);
    await connection.confirmTransaction(
      { signature: sig, ...(await connection.getLatestBlockhash()) },
      "confirmed",
    );
    console.log("created treasury ATAs, signature:", sig);
  }

  // 2. Point FeeConfig at them.
  const data = Buffer.concat([
    anchorDiscriminator("update_fee_config"),
    encodeParams(),
  ]);
  const keys = [
    { pubkey: admin.publicKey, isSigner: true, isWritable: false },
    { pubkey: feeConfig, isSigner: false, isWritable: true },
    { pubkey: SETTLEMENT_MINT, isSigner: false, isWritable: false },
    { pubkey: atas.dev, isSigner: false, isWritable: false },
    { pubkey: atas.ecosystem, isSigner: false, isWritable: false },
    { pubkey: atas.infra, isSigner: false, isWritable: false },
    { pubkey: atas.emergency, isSigner: false, isWritable: false },
  ];
  const tx = new Transaction().add(
    new TransactionInstruction({ programId: ESCROW_PROGRAM_ID, keys, data }),
  );
  const sig = await connection.sendTransaction(tx, [admin]);
  await connection.confirmTransaction(
    { signature: sig, ...(await connection.getLatestBlockhash()) },
    "confirmed",
  );
  console.log("update_fee_config signature:", sig);

  for (const [name, ata] of Object.entries(atas)) {
    console.log(`${name.padEnd(10)} treasury ATA: ${ata.toBase58()}`);
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
