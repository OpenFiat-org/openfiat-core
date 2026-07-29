/**
 * Live devnet proof that the escrow release path works and collects fees.
 *
 * Runs the real sequence against the deployed program — create liquidity
 * vault, deposit, reserve, create + fund trade escrow, approve, release —
 * and asserts the buyer and all four treasuries received exactly the
 * amounts `compute_fee_split` specifies.
 *
 *   ANCHOR_PROVIDER_URL=https://api.devnet.solana.com \
 *   ANCHOR_WALLET=~/.config/solana/id.json \
 *   npx ts-node scripts/prove-devnet-escrow-release.ts
 *
 * Each run uses a fresh reservation id, so it can be repeated.
 */
import * as anchor from "@anchor-lang/core";
import { Program, BN } from "@anchor-lang/core";
import { Escrow } from "../target/types/escrow";
import {
  TOKEN_2022_PROGRAM_ID,
  getAssociatedTokenAddressSync,
  createAssociatedTokenAccountInstruction,
  createMintToInstruction,
  getAccount,
} from "@solana/spl-token";
import {
  Keypair,
  PublicKey,
  SystemProgram,
  SYSVAR_RENT_PUBKEY,
  Transaction,
} from "@solana/web3.js";
import { readFileSync } from "node:fs";
import { join } from "node:path";

const MINT = new PublicKey(
  process.env.SETTLEMENT_MINT ?? "SK1JEbfsjjTG2WELNirmM7iJVcdnwerqfF32kCnoWsM",
);
/** 1,000 USDC at 6dp — large enough that a 15 bps fee splits without collapsing to zero. */
const AMOUNT = new BN("1000000000");
const OWNERS = {
  dev: new PublicKey("HLEB5akyStXEZfsTtgpnzexC4gyvDS5QNiYm1vDSHX4p"),
  ecosystem: new PublicKey("VueuQemTWZcZXMhfN1jRH1vs1zeWXYvzhDPC8uxJARF"),
  infra: new PublicKey("Aj2ciiPQJcun6fMcr1WCmcbKrMkyRsKrbFeDf2KKqySZ"),
  emergency: new PublicKey("836SJH2LfCgzV4rseMgGSTTPTTM3mMjkXp2exgkry6SH"),
};

async function main() {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const connection = provider.connection;
  const admin = (provider.wallet as anchor.Wallet).payer;
  const idl = JSON.parse(
    readFileSync(join(__dirname, "../target/idl/escrow.json"), "utf8"),
  ) as Escrow;
  const program = new Program<Escrow>(idl, provider);

  const merchant = admin;
  const buyer = Keypair.generate();
  const reservationId = new BN(Date.now());
  const idBytes = reservationId.toArrayLike(Buffer, "le", 8);

  const ata = (owner: PublicKey) =>
    getAssociatedTokenAddressSync(MINT, owner, true, TOKEN_2022_PROGRAM_ID);
  const pda = (seeds: (Buffer | Uint8Array)[]) =>
    PublicKey.findProgramAddressSync(seeds, program.programId)[0];

  const feeConfig = pda([Buffer.from("fee_config")]);
  const liquidityVault = pda([
    Buffer.from("liquidity_vault"), merchant.publicKey.toBuffer(), MINT.toBuffer(),
  ]);
  const liquidityTokens = pda([
    Buffer.from("liquidity_vault_tokens"), merchant.publicKey.toBuffer(), MINT.toBuffer(),
  ]);
  const tradeEscrow = pda([Buffer.from("trade_escrow"), idBytes]);
  const tradeTokens = pda([Buffer.from("trade_escrow_tokens"), idBytes]);

  const treasuries = Object.fromEntries(
    Object.entries(OWNERS).map(([k, o]) => [k, ata(o)]),
  ) as Record<keyof typeof OWNERS, PublicKey>;
  const merchantAta = ata(merchant.publicKey);
  const buyerAta = ata(buyer.publicKey);

  const balance = async (a: PublicKey) => {
    try {
      return (await getAccount(connection, a, "confirmed", TOKEN_2022_PROGRAM_ID)).amount;
    } catch {
      return 0n;
    }
  };

  /** Devnet intermittently rejects a just-fetched blockhash; same race the
   *  anchor suite already retries around. */
  const retry = async <T>(fn: () => Promise<T>, attempts = 6): Promise<T> => {
    for (let i = 0; i < attempts; i++) {
      try {
        return await fn();
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        const transient =
          msg.includes("Blockhash not found") ||
          msg.includes("block height exceeded") ||
          msg.includes("was not confirmed");
        if (!transient || i === attempts - 1) throw err;
        await new Promise((r) => setTimeout(r, 1500));
      }
    }
    throw new Error("unreachable");
  };

  console.log("reservation id:", reservationId.toString());
  console.log("settlement mint:", MINT.toBase58());

  // Fund the merchant, create the buyer's receiving account.
  const setup = new Transaction();
  if (!(await connection.getAccountInfo(merchantAta))) {
    setup.add(createAssociatedTokenAccountInstruction(
      admin.publicKey, merchantAta, merchant.publicKey, MINT, TOKEN_2022_PROGRAM_ID));
  }
  setup.add(
    createAssociatedTokenAccountInstruction(
      admin.publicKey, buyerAta, buyer.publicKey, MINT, TOKEN_2022_PROGRAM_ID),
    createMintToInstruction(
      MINT, merchantAta, admin.publicKey, BigInt(AMOUNT.toString()), [], TOKEN_2022_PROGRAM_ID),
  );
  await retry(() => provider.sendAndConfirm(setup, []));
  console.log("funded merchant with", AMOUNT.toString(), "base units");

  const before = {
    buyer: await balance(buyerAta),
    dev: await balance(treasuries.dev),
    ecosystem: await balance(treasuries.ecosystem),
    infra: await balance(treasuries.infra),
    emergency: await balance(treasuries.emergency),
  };

  if (!(await connection.getAccountInfo(liquidityVault))) {
    await retry(() => program.methods.createLiquidityVault().accountsPartial({
      merchant: merchant.publicKey,
      mint: MINT,
      liquidityVault,
      tokenVault: liquidityTokens,
      tokenProgram: TOKEN_2022_PROGRAM_ID,
      systemProgram: SystemProgram.programId,
      rent: SYSVAR_RENT_PUBKEY,
    }).rpc({ commitment: "confirmed" }));
    console.log("created liquidity vault");
  }

  await retry(() => program.methods.depositLiquidity(AMOUNT).accountsPartial({
    merchant: merchant.publicKey,
    liquidityVault,
    tokenVault: liquidityTokens,
    from: merchantAta,
    mint: MINT,
    tokenProgram: TOKEN_2022_PROGRAM_ID,
  }).rpc({ commitment: "confirmed" }));

  await retry(() => program.methods.reserveLiquidity(AMOUNT).accountsPartial({
    merchant: merchant.publicKey,
    liquidityVault,
  }).rpc({ commitment: "confirmed" }));
  console.log("deposited + reserved");

  await retry(() => program.methods
    .createTradeEscrow(reservationId, AMOUNT, new BN(1800))
    .accountsPartial({
      merchant: merchant.publicKey,
      buyer: buyer.publicKey,
      mint: MINT,
      liquidityVault,
      tradeEscrow,
      tokenVault: tradeTokens,
      tokenProgram: TOKEN_2022_PROGRAM_ID,
      systemProgram: SystemProgram.programId,
      rent: SYSVAR_RENT_PUBKEY,
    }).rpc({ commitment: "confirmed" }));

  await retry(() => program.methods.fundTradeEscrow().accountsPartial({
    merchant: merchant.publicKey,
    mint: MINT,
    liquidityVault,
    liquidityTokenVault: liquidityTokens,
    tradeEscrow,
    tradeEscrowTokenVault: tradeTokens,
    tokenProgram: TOKEN_2022_PROGRAM_ID,
  }).rpc({ commitment: "confirmed" }));

  await retry(() => program.methods.approveSettlement().accountsPartial({
    merchant: merchant.publicKey,
    tradeEscrow,
  }).rpc({ commitment: "confirmed" }));
  console.log("trade escrow created, funded, approved");

  const releaseSig = await retry(() => program.methods.releaseEscrow().accountsPartial({
    mint: MINT,
    liquidityVault,
    tradeEscrow,
    tradeEscrowTokenVault: tradeTokens,
    buyerTokenAccount: buyerAta,
    feeConfig,
    devTreasury: treasuries.dev,
    ecosystemTreasury: treasuries.ecosystem,
    infraTreasury: treasuries.infra,
    emergencyReserve: treasuries.emergency,
    tokenProgram: TOKEN_2022_PROGRAM_ID,
  }).rpc({ commitment: "confirmed" }));
  console.log("RELEASE SIGNATURE:", releaseSig);

  const after = {
    buyer: await balance(buyerAta),
    dev: await balance(treasuries.dev),
    ecosystem: await balance(treasuries.ecosystem),
    infra: await balance(treasuries.infra),
    emergency: await balance(treasuries.emergency),
  };

  // Mirrors compute_fee_split: 15 bps of the amount, then 40/30/20/10 of the
  // fee, with the truncation remainder swept to the emergency reserve.
  const amount = BigInt(AMOUNT.toString());
  const fee = (amount * 15n) / 10_000n;
  const expected: Record<string, bigint> = {
    buyer: amount - fee,
    dev: (fee * 4000n) / 10_000n,
    ecosystem: (fee * 3000n) / 10_000n,
    infra: (fee * 2000n) / 10_000n,
    emergency: (fee * 1000n) / 10_000n,
  };
  expected.emergency +=
    fee - (expected.dev + expected.ecosystem + expected.infra + expected.emergency);

  let ok = true;
  console.log(`\ntrade amount ${amount} base units, fee ${fee} (15 bps)`);
  for (const k of ["buyer", "dev", "ecosystem", "infra", "emergency"] as const) {
    const delta = after[k] - before[k];
    const pass = delta === expected[k];
    ok &&= pass;
    console.log(
      `  ${k.padEnd(10)} +${delta.toString().padStart(12)}  expected +${expected[k]
        .toString().padStart(12)}  ${pass ? "OK" : "MISMATCH"}`,
    );
  }
  if (!ok) {
    console.error("\nFEE SPLIT MISMATCH");
    process.exit(1);
  }
  console.log("\nfee split verified on devnet");
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
