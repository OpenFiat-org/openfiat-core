/**
 * One-off: raise the devnet presale's caps to production-scale figures
 * (OFS-4100 §3's proposed hard/soft cap) with a $1,000,000 per-wallet max,
 * via the new admin-only update_sale_params instruction (see
 * programs/presale/src/instructions/update_sale_params.rs).
 *
 * Usage: npx ts-node scripts/update-devnet-sale-params.ts
 */
import * as fs from "fs";
import * as path from "path";
import { Connection, Keypair, PublicKey, clusterApiUrl } from "@solana/web3.js";
import * as anchor from "@anchor-lang/core";
import { BN } from "@anchor-lang/core";

const PRESALE_PROGRAM_ID = new PublicKey(
  "75rJ9MRAaSnAc8tg4AfeTFVDCVrN6jdD5CqeyE4UoUw7",
);
const SALE_NONCE = 0;
const USDC_DECIMALS = 6;

async function main() {
  const connection = new Connection(clusterApiUrl("devnet"), "confirmed");
  const keypairPath =
    process.env.SOLANA_KEYPAIR ||
    path.join(process.env.HOME || "~", ".config/solana/id.json");
  const admin = Keypair.fromSecretKey(
    Uint8Array.from(JSON.parse(fs.readFileSync(keypairPath, "utf-8"))),
  );

  const wallet = new anchor.Wallet(admin);
  const provider = new anchor.AnchorProvider(connection, wallet, {
    commitment: "confirmed",
  });
  const idl = JSON.parse(
    fs.readFileSync(
      path.join(__dirname, "..", "target", "idl", "presale.json"),
      "utf-8",
    ),
  );
  const program = new anchor.Program(idl, provider) as anchor.Program<any>;

  const [saleConfig] = PublicKey.findProgramAddressSync(
    [Buffer.from("sale_config"), new BN(SALE_NONCE).toArrayLike(Buffer, "le", 8)],
    PRESALE_PROGRAM_ID,
  );

  const usdcUnit = (n: number) => new BN(n).mul(new BN(10).pow(new BN(USDC_DECIMALS)));

  const before = await (program.account as any).saleConfig.fetch(saleConfig);
  console.log("Before:", {
    hardCap: before.hardCap.toString(),
    softCap: before.softCap.toString(),
    minContribution: before.minContribution.toString(),
    maxContribution: before.maxContribution.toString(),
  });

  await program.methods
    .updateSaleParams(new BN(SALE_NONCE), {
      hardCap: usdcUnit(30_000_000),
      softCap: usdcUnit(5_000_000),
      minContribution: usdcUnit(1),
      maxContribution: usdcUnit(1_000_000),
      maxSlippageBps: 100,
    })
    .accountsPartial({ admin: admin.publicKey, saleConfig })
    .rpc({ commitment: "confirmed" });

  const after = await (program.account as any).saleConfig.fetch(saleConfig);
  console.log("After:", {
    hardCap: after.hardCap.toString(),
    softCap: after.softCap.toString(),
    minContribution: after.minContribution.toString(),
    maxContribution: after.maxContribution.toString(),
  });
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
