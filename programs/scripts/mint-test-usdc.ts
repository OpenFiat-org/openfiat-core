/**
 * One-off: mint devnet test-USDC (see init-devnet-sale.ts) to a tester's
 * wallet so they can exercise the direct-USDC contribute_usdc path in a
 * real browser. The mint's authority is deliberately live for exactly this.
 *
 * Usage: npx ts-node scripts/mint-test-usdc.ts <recipient> <amount>
 */
import * as fs from "fs";
import * as path from "path";
import { Connection, Keypair, PublicKey, clusterApiUrl } from "@solana/web3.js";
import {
  TOKEN_2022_PROGRAM_ID,
  getOrCreateAssociatedTokenAccount,
  mintTo,
} from "@solana/spl-token";

const USDC_DECIMALS = 6;

async function main() {
  const [recipientArg, amountArg] = process.argv.slice(2);
  if (!recipientArg || !amountArg) {
    throw new Error("Usage: mint-test-usdc.ts <recipient> <amount>");
  }
  const recipient = new PublicKey(recipientArg);
  const amount = Number(amountArg);

  const connection = new Connection(clusterApiUrl("devnet"), "confirmed");
  const keypairPath =
    process.env.SOLANA_KEYPAIR ||
    path.join(process.env.HOME || "~", ".config/solana/id.json");
  const admin = Keypair.fromSecretKey(
    Uint8Array.from(JSON.parse(fs.readFileSync(keypairPath, "utf-8"))),
  );

  const addrPath = path.join(__dirname, "..", "devnet-addresses.json");
  const allAddresses = JSON.parse(fs.readFileSync(addrPath, "utf-8"));
  const usdcMint = new PublicKey(allAddresses.devnet_sale.usdcMint);

  const ata = await getOrCreateAssociatedTokenAccount(
    connection,
    admin,
    usdcMint,
    recipient,
    false,
    undefined,
    undefined,
    TOKEN_2022_PROGRAM_ID,
  );

  const units = BigInt(Math.round(amount * 10 ** USDC_DECIMALS));
  const sig = await mintTo(
    connection,
    admin,
    usdcMint,
    ata.address,
    admin,
    units,
    [],
    { commitment: "confirmed" },
    TOKEN_2022_PROGRAM_ID,
  );

  console.log(`Minted ${amount} test-USDC to ${recipient.toBase58()}`);
  console.log(`Token account: ${ata.address.toBase58()}`);
  console.log(`Signature: ${sig}`);
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
