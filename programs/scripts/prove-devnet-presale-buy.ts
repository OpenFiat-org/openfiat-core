/**
 * End-to-end proof of the normal on-chain presale BUY path (the one the app's
 * "Buy OPEN" button drives): a fresh buyer contributes 1 USDC via
 * `contribute_usdc` and then `claim`s, receiving exactly 100 OPEN — proving
 * the re-baselined 1 USDC = 100 OPEN rate live on devnet against the deployed
 * SaleConfig (nonce 1).
 *
 *   npx ts-node scripts/prove-devnet-presale-buy.ts
 */
import * as fs from "fs";
import * as path from "path";
import * as anchor from "@anchor-lang/core";
import { BN } from "@anchor-lang/core";
import {
  Connection,
  Keypair,
  PublicKey,
  SystemProgram,
} from "@solana/web3.js";
import {
  TOKEN_2022_PROGRAM_ID,
  getOrCreateAssociatedTokenAccount,
  getAssociatedTokenAddressSync,
  mintTo,
  getAccount,
} from "@solana/spl-token";

const RPC = process.env.SOLANA_RPC_URL ?? "https://api.devnet.solana.com";
const BAN_SEED = Buffer.from("ban");
const GOVERNANCE_PROGRAM_ID = new PublicKey("2k71DBDoxM4SUFYGbyMXFiTSUynPuY2CqFUsx3FuarXF");
const CONTRIBUTION_SEED = Buffer.from("contribution");
const PRESALE_VAULT_SEED = Buffer.from("presale_vault");

function loadKp(p: string): Keypair {
  return Keypair.fromSecretKey(Uint8Array.from(JSON.parse(fs.readFileSync(p, "utf8"))));
}

async function main() {
  const connection = new Connection(RPC, "confirmed");
  const funder = loadKp(path.join(process.env.HOME!, ".config", "solana", "id.json")); // EA8Ty
  const addrs = JSON.parse(fs.readFileSync(path.join(__dirname, "..", "devnet-addresses.json"), "utf8"));
  const sale = addrs.devnet_sale;
  const programId = new PublicKey(sale.programId);
  const saleNonce = new BN(sale.saleNonce);
  const openMint = new PublicKey(sale.openMint);
  const usdcMint = new PublicKey(sale.usdcMint);
  const usdcVault = new PublicKey(sale.usdcVault);
  const saleConfig = new PublicKey(sale.saleConfig);
  const presaleVault = new PublicKey(addrs.devnet["bucket_community-presale"]);

  const idl = JSON.parse(fs.readFileSync(path.join(__dirname, "..", "target", "idl", "presale.json"), "utf8"));

  // A fresh buyer, funded a little SOL for fees + ATA/PDA rents.
  const buyer = Keypair.generate();
  const fund = new anchor.web3.Transaction().add(
    SystemProgram.transfer({ fromPubkey: funder.publicKey, toPubkey: buyer.publicKey, lamports: 0.05 * 1e9 }),
  );
  await anchor.web3.sendAndConfirmTransaction(connection, fund, [funder]);

  const provider = new anchor.AnchorProvider(connection, new anchor.Wallet(buyer), { commitment: "confirmed" });
  const program = new anchor.Program(idl, provider);

  const [banRecord] = PublicKey.findProgramAddressSync([BAN_SEED, buyer.publicKey.toBuffer()], GOVERNANCE_PROGRAM_ID);
  const [contribution] = PublicKey.findProgramAddressSync(
    [CONTRIBUTION_SEED, saleConfig.toBuffer(), buyer.publicKey.toBuffer()], programId);
  const [presaleVaultAuthority] = PublicKey.findProgramAddressSync([PRESALE_VAULT_SEED], programId);

  // The buyer's USDC, minted from the devnet test-USDC (funder holds mint authority).
  const buyerUsdc = await getOrCreateAssociatedTokenAccount(
    connection, funder, usdcMint, buyer.publicKey, false, "confirmed", undefined, TOKEN_2022_PROGRAM_ID);
  const ONE_USDC = 1_000_000;
  await mintTo(connection, funder, usdcMint, buyerUsdc.address, funder, ONE_USDC, [], undefined, TOKEN_2022_PROGRAM_ID);

  // The buyer's OPEN ATA must exist before claim (claim requires it initialized;
  // the app creates it as part of the buy flow). Funder pays the rent.
  const buyerOpenAcc = await getOrCreateAssociatedTokenAccount(
    connection, funder, openMint, buyer.publicKey, false, "confirmed", undefined, TOKEN_2022_PROGRAM_ID);
  const buyerOpen = buyerOpenAcc.address;

  const assert = (name: string, got: bigint | string, want: bigint | string) => {
    if (got.toString() !== want.toString()) throw new Error(`${name}: got ${got}, want ${want}`);
    console.log(`  ✓ ${name} = ${got}`);
  };

  console.log(`buyer: ${buyer.publicKey.toBase58()}`);
  console.log("[1/2] contribute_usdc(1 USDC)");
  await program.methods
    .contributeUsdc(saleNonce, new BN(ONE_USDC))
    .accounts({
      buyer: buyer.publicKey, banRecord, saleConfig, buyerUsdc: buyerUsdc.address,
      usdcVault, usdcMint, contribution,
      tokenProgram: TOKEN_2022_PROGRAM_ID, systemProgram: SystemProgram.programId,
    }).rpc();
  const c = await (program.account as any).contribution.fetch(contribution);
  assert("contribution.amount_usdc", BigInt(c.amountUsdc.toString()), BigInt(ONE_USDC));
  assert("contribution.open_entitlement (100 OPEN)", BigInt(c.openEntitlement.toString()), BigInt(ONE_USDC) * 100n);

  console.log("[2/2] claim -> buyer receives 100 OPEN");
  await program.methods
    .claim(saleNonce)
    .accounts({
      buyer: buyer.publicKey, saleConfig, openMint, presaleVaultAuthority, presaleVault,
      contribution, buyerOpen, tokenProgram: TOKEN_2022_PROGRAM_ID,
    }).rpc();
  const bal = (await getAccount(connection, buyerOpen, "confirmed", TOKEN_2022_PROGRAM_ID)).amount;
  assert("buyer OPEN balance (100 OPEN @ 6dec)", bal, BigInt(ONE_USDC) * 100n);

  const cfg = await (program.account as any).saleConfig.fetch(saleConfig);
  console.log(`\nsale total_raised: ${cfg.totalRaised.toString()} USDC base units (openPerUsdc=${cfg.openPerUsdc.toString()})`);
  console.log("DONE — presale BUY path proven live: 1 USDC -> 100 OPEN, claimed.");
}

main().then(() => process.exit(0), (e) => { console.error(e); process.exit(1); });
