/**
 * One-off: unblock claim-flow testing on the devnet presale.
 *
 * The sale's soft cap (5,000,000 USDC) and end_time (a month out) were both
 * set for a production-scale sale, but only ~1,000 USDC has actually been
 * raised in this devnet test pass — nowhere near enough to finalize
 * naturally, which meant `claim` could never be exercised end-to-end.
 * Lowers soft_cap below what's already raised and moves end_time into the
 * past via update_sale_params, then calls finalize_sale so the sale
 * resolves to Finalized (not SoftCapMissed) and existing contributors can
 * actually claim.
 *
 * This finalizes the sale — no further contributions are possible under
 * this sale_nonce afterward. That's the intended tradeoff here: unblocking
 * the claim test that's actually in progress takes priority over keeping
 * this specific round open for more contributions.
 *
 * Usage: npx ts-node scripts/finalize-devnet-sale-for-testing.ts
 */
import * as fs from "fs";
import * as path from "path";
import { Connection, Keypair, PublicKey, clusterApiUrl } from "@solana/web3.js";
import { TOKEN_2022_PROGRAM_ID, getOrCreateAssociatedTokenAccount } from "@solana/spl-token";
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
    state: JSON.stringify(before.state),
    totalRaised: before.totalRaised.toString(),
    softCap: before.softCap.toString(),
    endTime: new Date(before.endTime.toNumber() * 1000).toISOString(),
  });

  const now = Math.floor(Date.now() / 1000);
  await program.methods
    .updateSaleParams(new BN(SALE_NONCE), {
      hardCap: before.hardCap,
      softCap: usdcUnit(100), // well below the ~1,000 USDC already raised
      minContribution: before.minContribution,
      maxContribution: before.maxContribution,
      maxSlippageBps: before.maxSlippageBps,
      endTime: new BN(now - 60), // already past, so finalize becomes callable
    })
    .accountsPartial({ admin: admin.publicKey, saleConfig })
    .rpc({ commitment: "confirmed" });
  console.log("Lowered soft_cap and moved end_time into the past.");

  const usdcMint = new PublicKey(before.usdcMint);
  const treasury = await getOrCreateAssociatedTokenAccount(
    connection,
    admin,
    usdcMint,
    admin.publicKey,
    false,
    undefined,
    { commitment: "confirmed" },
    TOKEN_2022_PROGRAM_ID,
  );

  await program.methods
    .finalizeSale(new BN(SALE_NONCE))
    .accountsPartial({
      admin: admin.publicKey,
      saleConfig,
      usdcVault: before.usdcVault,
      treasury: treasury.address,
      usdcMint,
      tokenProgram: TOKEN_2022_PROGRAM_ID,
    })
    .rpc({ commitment: "confirmed" });

  const after = await (program.account as any).saleConfig.fetch(saleConfig);
  console.log("After finalize:", {
    state: JSON.stringify(after.state),
    totalRaised: after.totalRaised.toString(),
  });
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
