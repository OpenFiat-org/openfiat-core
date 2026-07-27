/**
 * One-time devnet setup for browser end-to-end testing of the direct-USDC
 * contribution path (OFS-4100 §3, OFS-4200 §3).
 *
 * Creates a devnet-only test-USDC mint (Token-2022, 6 decimals — there is no
 * canonical devnet USDC; OFS-4100 §3 explicitly anticipates "devnet
 * equivalents/test mints during the devnet phase of this build"). Unlike the
 * OPEN mint, this mint's authority is deliberately left live so test USDC can
 * be minted to any tester's wallet on request.
 *
 * Sale terms here are intentionally small/practical for a browser
 * click-through test, NOT OFS-4100 §3's proposed production figures (a real
 * $30M hard cap has nothing to click-test against on devnet). The swap
 * path's aggregator is still pointed at Jupiter's real, verified program id
 * for production-representativeness, even though it isn't exercised in this
 * pass — see the decision log in the Phase 3 session for why the SOL/
 * stablecoin-via-Jupiter path isn't practically testable against real
 * infra on devnet.
 *
 * Usage: npx ts-node scripts/init-devnet-sale.ts
 */
import * as fs from "fs";
import * as path from "path";
import {
  Connection,
  Keypair,
  PublicKey,
  SYSVAR_RENT_PUBKEY,
  clusterApiUrl,
} from "@solana/web3.js";
import { TOKEN_2022_PROGRAM_ID, createMint, getOrCreateAssociatedTokenAccount } from "@solana/spl-token";
import * as anchor from "@anchor-lang/core";
import { BN } from "@anchor-lang/core";

const PRESALE_PROGRAM_ID = new PublicKey(
  "75rJ9MRAaSnAc8tg4AfeTFVDCVrN6jdD5CqeyE4UoUw7",
);
const JUPITER_V6_PROGRAM_ID = new PublicKey(
  "JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4",
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
  console.log(`Admin pubkey: ${admin.publicKey.toBase58()}`);

  const addrPath = path.join(__dirname, "..", "devnet-addresses.json");
  const allAddresses = JSON.parse(fs.readFileSync(addrPath, "utf-8"));
  const genesis = allAddresses.devnet;
  if (!genesis) {
    throw new Error("No devnet genesis addresses found — run genesis.ts --cluster devnet first.");
  }
  const openMint = new PublicKey(genesis.mint);
  const presaleVault = new PublicKey(genesis["bucket_community-presale"]);

  console.log("\nCreating devnet test-USDC mint (Token-2022, 6 decimals)...");
  const usdcMint = await createMint(
    connection,
    admin,
    admin.publicKey, // mint authority — deliberately NOT revoked; this is a test faucet mint
    admin.publicKey,
    USDC_DECIMALS,
    undefined,
    undefined,
    TOKEN_2022_PROGRAM_ID,
  );
  console.log(`Test USDC mint: ${usdcMint.toBase58()}`);

  const treasury = await getOrCreateAssociatedTokenAccount(
    connection,
    admin,
    usdcMint,
    admin.publicKey,
    false,
    undefined,
    undefined,
    TOKEN_2022_PROGRAM_ID,
  );
  console.log(`Treasury (admin's own USDC ATA): ${treasury.address.toBase58()}`);

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
  const program = new anchor.Program(idl, provider);

  const [saleConfig] = PublicKey.findProgramAddressSync(
    [Buffer.from("sale_config"), new BN(SALE_NONCE).toArrayLike(Buffer, "le", 8)],
    PRESALE_PROGRAM_ID,
  );
  const [usdcVault] = PublicKey.findProgramAddressSync(
    [Buffer.from("sale_usdc_vault"), new BN(SALE_NONCE).toArrayLike(Buffer, "le", 8)],
    PRESALE_PROGRAM_ID,
  );
  const [presaleVaultAuthority] = PublicKey.findProgramAddressSync(
    [Buffer.from("presale_vault")],
    PRESALE_PROGRAM_ID,
  );

  const existing = await connection.getAccountInfo(saleConfig);
  if (existing) {
    console.log(`\nsale_config already initialized at nonce ${SALE_NONCE} — skipping initialize_sale.`);
  } else {
    const now = Math.floor(Date.now() / 1000);
    const usdcUnit = (n: number) => new BN(n).mul(new BN(10).pow(new BN(USDC_DECIMALS)));
    console.log("\nCalling initialize_sale (small, testable devnet terms)...");
    await program.methods
      .initializeSale(new BN(SALE_NONCE), {
        hardCap: usdcUnit(1_000),
        softCap: usdcUnit(10),
        minContribution: usdcUnit(1),
        maxContribution: usdcUnit(500),
        maxSlippageBps: 100,
        startTime: new BN(now - 60),
        endTime: new BN(now + 60 * 60 * 24 * 30), // 30 days
        stablecoinWhitelist: [],
      })
      .accountsPartial({
        admin: admin.publicKey,
        saleConfig,
        openMint,
        usdcMint,
        presaleVaultAuthority,
        presaleVault,
        usdcVault,
        treasury: treasury.address,
        swapProgram: JUPITER_V6_PROGRAM_ID,
        tokenProgram: TOKEN_2022_PROGRAM_ID,
        systemProgram: anchor.web3.SystemProgram.programId,
        rent: SYSVAR_RENT_PUBKEY,
      })
      .rpc({ commitment: "confirmed" });
    console.log("Sale initialized.");
  }

  const out = {
    programId: PRESALE_PROGRAM_ID.toBase58(),
    saleNonce: SALE_NONCE,
    openMint: openMint.toBase58(),
    usdcMint: usdcMint.toBase58(),
    swapProgram: JUPITER_V6_PROGRAM_ID.toBase58(),
    saleConfig: saleConfig.toBase58(),
    usdcVault: usdcVault.toBase58(),
    treasury: treasury.address.toBase58(),
    hardCapUsdc: 1_000,
    softCapUsdc: 10,
    minContributionUsdc: 1,
    maxContributionUsdc: 500,
    opensAt: new Date((Math.floor(Date.now() / 1000) - 60) * 1000).toISOString(),
    closesAt: new Date((Math.floor(Date.now() / 1000) + 60 * 60 * 24 * 30) * 1000).toISOString(),
  };
  allAddresses.devnet_sale = out;
  fs.writeFileSync(addrPath, JSON.stringify(allAddresses, null, 2) + "\n");
  console.log("\nWrote devnet_sale entry to devnet-addresses.json:");
  console.log(JSON.stringify(out, null, 2));
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
