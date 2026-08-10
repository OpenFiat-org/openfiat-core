/**
 * Devnet proof for SP-B `deliver_contribution`: stands in for the deBridge
 * executor. Mints test-USDC into a `source_usdc` account, submits the exact
 * single instruction a deBridge Solana Hook would (deliver_contribution),
 * and asserts the cross-chain contribution is credited AND the OPEN is
 * auto-delivered to the bound recipient — then proves the no-free-mint guard
 * by attempting a delivery with no USDC and asserting it reverts with nothing
 * recorded.
 *
 *   npx ts-node scripts/prove-devnet-deliver-contribution.ts
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
  ASSOCIATED_TOKEN_PROGRAM_ID,
  getAssociatedTokenAddressSync,
  getOrCreateAssociatedTokenAccount,
  mintTo,
  getAccount,
} from "@solana/spl-token";

const RPC = process.env.SOLANA_RPC_URL ?? "https://api.devnet.solana.com";
const BAN_SEED = Buffer.from("ban");
const GOVERNANCE_PROGRAM_ID = new PublicKey("2k71DBDoxM4SUFYGbyMXFiTSUynPuY2CqFUsx3FuarXF");
const CONTRIBUTION_SEED = Buffer.from("contribution");
const PRESALE_VAULT_SEED = Buffer.from("presale_vault");
const SALE_CONFIG_SEED = Buffer.from("sale_config");

function loadKp(p: string): Keypair {
  return Keypair.fromSecretKey(Uint8Array.from(JSON.parse(fs.readFileSync(p, "utf8"))));
}

async function main() {
  const connection = new Connection(RPC, "confirmed");
  const payer = loadKp(path.join(process.env.HOME!, ".config", "solana", "id.json")); // EA8Ty = mock executor
  const addrs = JSON.parse(fs.readFileSync(path.join(__dirname, "..", "devnet-addresses.json"), "utf8"));
  const sale = addrs.devnet_sale;
  const presaleProgramId = new PublicKey(sale.programId);
  const saleNonce = new BN(sale.saleNonce);
  const openMint = new PublicKey(sale.openMint);
  const usdcMint = new PublicKey(sale.usdcMint);
  const usdcVault = new PublicKey(sale.usdcVault);
  const saleConfig = new PublicKey(sale.saleConfig);
  const presaleVault = new PublicKey(sale.presaleVault ?? addrs.devnet["bucket_community-presale"]);

  const idl = JSON.parse(fs.readFileSync(path.join(__dirname, "..", "target", "idl", "presale.json"), "utf8"));
  const provider = new anchor.AnchorProvider(connection, new anchor.Wallet(payer), { commitment: "confirmed" });
  const program = new anchor.Program(idl, provider);

  const [presaleVaultAuthority] = PublicKey.findProgramAddressSync([PRESALE_VAULT_SEED], presaleProgramId);

  // A fresh cross-chain recipient (a Solana wallet the buyer named).
  const recipient = Keypair.generate().publicKey;
  const [banRecord] = PublicKey.findProgramAddressSync([BAN_SEED, recipient.toBuffer()], GOVERNANCE_PROGRAM_ID);
  const [contribution] = PublicKey.findProgramAddressSync(
    [CONTRIBUTION_SEED, saleConfig.toBuffer(), recipient.toBuffer()], presaleProgramId);
  const recipientOpen = getAssociatedTokenAddressSync(openMint, recipient, true, TOKEN_2022_PROGRAM_ID);

  // The USDC the "DLN" delivered to the executor.
  const sourceUsdcAcc = await getOrCreateAssociatedTokenAccount(
    connection, payer, usdcMint, payer.publicKey, false, "confirmed", undefined, TOKEN_2022_PROGRAM_ID);
  const USDC = 1_000_000; // 1 USDC (6 dec)
  await mintTo(connection, payer, usdcMint, sourceUsdcAcc.address, payer, USDC, [], undefined, TOKEN_2022_PROGRAM_ID);

  const usdcVaultBefore = (await getAccount(connection, usdcVault, "confirmed", TOKEN_2022_PROGRAM_ID)).amount;

  console.log(`recipient: ${recipient.toBase58()}`);
  console.log("[1/2] Happy path: deliver_contribution(1 USDC) -> 100 OPEN auto-delivered");
  const sig = await program.methods
    .deliverContribution(saleNonce, recipient, new BN(USDC))
    .accounts({
      payer: payer.publicKey,
      banRecord,
      saleConfig,
      usdcMint,
      sourceUsdc: sourceUsdcAcc.address,
      usdcVault,
      openMint,
      presaleVaultAuthority,
      presaleVault,
      recipientAccount: recipient,
      recipientOpen,
      contribution,
      usdcTokenProgram: TOKEN_2022_PROGRAM_ID,
      openTokenProgram: TOKEN_2022_PROGRAM_ID,
      associatedTokenProgram: ASSOCIATED_TOKEN_PROGRAM_ID,
      systemProgram: SystemProgram.programId,
    })
    .rpc();
  console.log(`  submitted ${sig}`);

  const usdcVaultAfter = (await getAccount(connection, usdcVault, "confirmed", TOKEN_2022_PROGRAM_ID)).amount;
  const recvOpen = (await getAccount(connection, recipientOpen, "confirmed", TOKEN_2022_PROGRAM_ID)).amount;
  const c = await (program.account as any).contribution.fetch(contribution);

  const assert = (name: string, got: bigint | string, want: bigint | string) => {
    if (got.toString() !== want.toString()) throw new Error(`${name}: got ${got}, want ${want}`);
    console.log(`  ✓ ${name} = ${got}`);
  };
  assert("usdc_vault delta", usdcVaultAfter - usdcVaultBefore, BigInt(USDC));
  assert("contribution.amount_usdc", BigInt(c.amountUsdc.toString()), BigInt(USDC));
  assert("contribution.open_entitlement (100 OPEN)", BigInt(c.openEntitlement.toString()), BigInt(USDC) * 100n);
  assert("contribution.claimed_open (auto-delivered)", BigInt(c.claimedOpen.toString()), BigInt(USDC) * 100n);
  assert("recipient OPEN balance", recvOpen, BigInt(USDC) * 100n);

  console.log("[2/2] No-free-mint: deliver with an empty source_usdc must revert, nothing recorded");
  const recipient2 = Keypair.generate().publicKey;
  const [banRecord2] = PublicKey.findProgramAddressSync([BAN_SEED, recipient2.toBuffer()], GOVERNANCE_PROGRAM_ID);
  const [contribution2] = PublicKey.findProgramAddressSync(
    [CONTRIBUTION_SEED, saleConfig.toBuffer(), recipient2.toBuffer()], presaleProgramId);
  const recipientOpen2 = getAssociatedTokenAddressSync(openMint, recipient2, true, TOKEN_2022_PROGRAM_ID);
  const emptyUsdc = await getOrCreateAssociatedTokenAccount(
    connection, payer, usdcMint, Keypair.generate().publicKey, true, "confirmed", undefined, TOKEN_2022_PROGRAM_ID);
  let reverted = false;
  try {
    await program.methods
      .deliverContribution(saleNonce, recipient2, new BN(USDC))
      .accounts({
        payer: payer.publicKey, banRecord: banRecord2, saleConfig, usdcMint,
        sourceUsdc: emptyUsdc.address, usdcVault, openMint, presaleVaultAuthority, presaleVault,
        recipientAccount: recipient2, recipientOpen: recipientOpen2, contribution: contribution2,
        usdcTokenProgram: TOKEN_2022_PROGRAM_ID, openTokenProgram: TOKEN_2022_PROGRAM_ID,
        associatedTokenProgram: ASSOCIATED_TOKEN_PROGRAM_ID, systemProgram: SystemProgram.programId,
      }).rpc();
  } catch {
    reverted = true;
  }
  if (!reverted) throw new Error("no-free-mint: expected revert, transaction succeeded");
  const stillAbsent = await connection.getAccountInfo(contribution2);
  if (stillAbsent) throw new Error("no-free-mint: a Contribution was recorded despite the revert");
  console.log("  ✓ reverted with no USDC, no Contribution created, no OPEN moved");

  console.log("\nDONE — deliver_contribution proven live on devnet.");
}

main().then(() => process.exit(0), (e) => { console.error(e); process.exit(1); });
