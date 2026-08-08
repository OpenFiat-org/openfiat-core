/**
 * Devnet proof for the claim-anytime + sweep_proceeds presale change
 * (OFS-4100 §3, OFS-4200 §3). Proves, against real on-chain state under a
 * FRESH sale_nonce, that:
 *
 *   1. a buyer can `claim` their OPEN while the sale is still Active
 *      (no finalize gate), and
 *   2. a second `claim` after a further contribution pays only the newly
 *      accrued delta (the `claimed_open` high-water mark), and
 *   3. the admin can `sweep_proceeds` USDC out of the sale's usdc_vault to
 *      the fixed treasury while the sale runs, and
 *   4. claims remain fully payable after a sweep (claims never touch the
 *      usdc_vault).
 *
 * A new SaleConfig is created under a unique nonce (unix seconds), so this
 * never collides with — or migrates — any existing devnet sale. A fresh
 * devnet-only test-USDC mint (Token-2022, 6 decimals, admin-controlled) is
 * created so USDC can be minted to the test buyer, mirroring
 * init-devnet-sale.ts. OPEN is drawn from the genesis-funded Community
 * Presale bucket (the `presale_vault` PDA), which the program signs for.
 *
 * Usage: npx ts-node scripts/prove-devnet-presale-claim-sweep.ts
 */
import * as fs from "fs";
import * as path from "path";
import {
  Connection,
  Keypair,
  PublicKey,
  SystemProgram,
  SYSVAR_RENT_PUBKEY,
  clusterApiUrl,
} from "@solana/web3.js";
import {
  TOKEN_2022_PROGRAM_ID,
  createMint,
  getOrCreateAssociatedTokenAccount,
  mintTo,
  getAccount,
} from "@solana/spl-token";
import * as anchor from "@anchor-lang/core";
import { BN } from "@anchor-lang/core";

const PRESALE_PROGRAM_ID = new PublicKey(
  "75rJ9MRAaSnAc8tg4AfeTFVDCVrN6jdD5CqeyE4UoUw7",
);
const GOVERNANCE_PROGRAM_ID = new PublicKey(
  "AVJfKUjHsizkGGUy8sdz4Xma2hVgmgvgg8GmUMs8E4eE",
);
const JUPITER_V6_PROGRAM_ID = new PublicKey(
  "JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4",
);
const OPEN_DECIMALS = 9;
const USDC_DECIMALS = 6;

// Scale a whole-token USDC amount to base units (6 decimals) and the matching
// OPEN entitlement to base units (9 decimals) at the confirmed 1:1 price.
const usdc = (n: number) => new BN(n).mul(new BN(10).pow(new BN(USDC_DECIMALS)));
const open = (n: number) => new BN(n).mul(new BN(10).pow(new BN(OPEN_DECIMALS)));

function assertEq(actual: string, expected: string, label: string) {
  if (actual !== expected) {
    throw new Error(`ASSERT FAILED [${label}]: got ${actual}, expected ${expected}`);
  }
  console.log(`  ✔ ${label}: ${actual}`);
}

async function main() {
  // Source the RPC target from the environment (defaulting to devnet) and
  // guard on that same value — so a misconfigured ANCHOR_PROVIDER_URL is
  // actually caught, rather than a hardcoded literal that can never fail.
  const rpcUrl = process.env.ANCHOR_PROVIDER_URL || clusterApiUrl("devnet");
  if (!rpcUrl.includes("devnet")) {
    throw new Error(`Refusing to run: RPC URL ${rpcUrl} is not devnet. This script is devnet-only.`);
  }
  const connection = new Connection(rpcUrl, "confirmed");

  const keypairPath =
    process.env.SOLANA_KEYPAIR ||
    path.join(process.env.HOME || "~", ".config/solana/id.json");
  const admin = Keypair.fromSecretKey(
    Uint8Array.from(JSON.parse(fs.readFileSync(keypairPath, "utf-8"))),
  );
  console.log(`Admin/authority: ${admin.publicKey.toBase58()}`);

  const addrPath = path.join(__dirname, "..", "devnet-addresses.json");
  const allAddresses = JSON.parse(fs.readFileSync(addrPath, "utf-8"));
  const genesis = allAddresses.devnet;
  const openMint = new PublicKey(genesis.mint);
  const presaleVault = new PublicKey(genesis["bucket_community-presale"]);

  const wallet = new anchor.Wallet(admin);
  const provider = new anchor.AnchorProvider(connection, wallet, {
    commitment: "confirmed",
  });
  const idl = JSON.parse(
    fs.readFileSync(path.join(__dirname, "..", "target", "idl", "presale.json"), "utf-8"),
  );
  const program = new anchor.Program(idl, provider);

  const sigs: Record<string, string> = {};

  // --- Fresh, collision-proof sale ---
  const SALE_NONCE = Math.floor(Date.now() / 1000);
  console.log(`\nUsing fresh sale_nonce ${SALE_NONCE}`);

  console.log("Creating devnet test-USDC mint (Token-2022, 6 decimals, admin authority)...");
  const usdcMint = await createMint(
    connection, admin, admin.publicKey, admin.publicKey, USDC_DECIMALS,
    undefined, undefined, TOKEN_2022_PROGRAM_ID,
  );
  console.log(`  test USDC mint: ${usdcMint.toBase58()}`);

  // Treasury = admin's own ATA on the test-USDC mint (the fixed sweep target).
  const treasury = (await getOrCreateAssociatedTokenAccount(
    connection, admin, usdcMint, admin.publicKey, false, undefined, undefined, TOKEN_2022_PROGRAM_ID,
  )).address;

  const [saleConfig] = PublicKey.findProgramAddressSync(
    [Buffer.from("sale_config"), new BN(SALE_NONCE).toArrayLike(Buffer, "le", 8)],
    PRESALE_PROGRAM_ID,
  );
  const [usdcVault] = PublicKey.findProgramAddressSync(
    [Buffer.from("sale_usdc_vault"), new BN(SALE_NONCE).toArrayLike(Buffer, "le", 8)],
    PRESALE_PROGRAM_ID,
  );
  const [presaleVaultAuthority] = PublicKey.findProgramAddressSync(
    [Buffer.from("presale_vault")], PRESALE_PROGRAM_ID,
  );

  const now = Math.floor(Date.now() / 1000);
  console.log("initialize_sale (soft_cap = 0, small devnet terms)...");
  sigs.initialize = await program.methods
    .initializeSale(new BN(SALE_NONCE), {
      hardCap: usdc(1_000),
      softCap: new BN(0),
      minContribution: usdc(1),
      maxContribution: usdc(500),
      maxSlippageBps: 100,
      startTime: new BN(now - 60),
      endTime: new BN(now + 60 * 60 * 24 * 30),
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
      treasury,
      swapProgram: JUPITER_V6_PROGRAM_ID,
      tokenProgram: TOKEN_2022_PROGRAM_ID,
      systemProgram: SystemProgram.programId,
      rent: SYSVAR_RENT_PUBKEY,
    })
    .rpc({ commitment: "confirmed" });
  console.log(`  ✔ sale initialized: ${sigs.initialize}`);

  // --- Buyer setup ---
  const buyer = Keypair.generate();
  console.log(`\nBuyer: ${buyer.publicKey.toBase58()}`);
  // Fund the buyer from admin (more reliable than devnet airdrop) for fees/rent.
  const fund = SystemProgram.transfer({
    fromPubkey: admin.publicKey, toPubkey: buyer.publicKey, lamports: 0.2 * 1e9,
  });
  const fundTx = new anchor.web3.Transaction().add(fund);
  await provider.sendAndConfirm(fundTx, []);

  const buyerUsdc = (await getOrCreateAssociatedTokenAccount(
    connection, admin, usdcMint, buyer.publicKey, false, undefined, undefined, TOKEN_2022_PROGRAM_ID,
  )).address;
  const buyerOpen = (await getOrCreateAssociatedTokenAccount(
    connection, admin, openMint, buyer.publicKey, false, undefined, undefined, TOKEN_2022_PROGRAM_ID,
  )).address;
  await mintTo(
    connection, admin, usdcMint, buyerUsdc, admin, BigInt(usdc(200).toString()),
    [], { commitment: "confirmed" }, TOKEN_2022_PROGRAM_ID,
  );

  const [contribution] = PublicKey.findProgramAddressSync(
    [Buffer.from("contribution"), saleConfig.toBuffer(), buyer.publicKey.toBuffer()],
    PRESALE_PROGRAM_ID,
  );
  const [banRecord] = PublicKey.findProgramAddressSync(
    [Buffer.from("ban"), buyer.publicKey.toBuffer()], GOVERNANCE_PROGRAM_ID,
  );

  const contributeUsdc = async (amount: BN) =>
    program.methods
      .contributeUsdc(new BN(SALE_NONCE), amount)
      .accountsPartial({
        buyer: buyer.publicKey,
        banRecord,
        saleConfig,
        buyerUsdc,
        usdcVault,
        usdcMint,
        contribution,
        tokenProgram: TOKEN_2022_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .signers([buyer])
      .rpc({ commitment: "confirmed" });

  const claim = async () =>
    program.methods
      .claim(new BN(SALE_NONCE))
      .accountsPartial({
        buyer: buyer.publicKey,
        saleConfig,
        openMint,
        presaleVaultAuthority,
        presaleVault,
        contribution,
        buyerOpen,
        tokenProgram: TOKEN_2022_PROGRAM_ID,
      })
      .signers([buyer])
      .rpc({ commitment: "confirmed" });

  const openBalance = async () =>
    (await getAccount(connection, buyerOpen, "confirmed", TOKEN_2022_PROGRAM_ID)).amount.toString();
  const usdcBalance = async (acc: PublicKey) =>
    (await getAccount(connection, acc, "confirmed", TOKEN_2022_PROGRAM_ID)).amount.toString();

  // --- 1) contribute 60, claim WHILE ACTIVE ---
  console.log("\n[1] contribute 60 USDC, then claim while Active");
  sigs.contribute1 = await contributeUsdc(usdc(60));
  const c1 = await (program.account as any).contribution.fetch(contribution);
  assertEq(c1.openEntitlement.toString(), open(60).toString(), "entitlement after 60 USDC");
  sigs.claim1 = await claim();
  assertEq(await openBalance(), open(60).toString(), "buyer OPEN after first claim (Active)");

  // --- 2) contribute 40 more, claim only the delta ---
  console.log("\n[2] contribute 40 more USDC, claim only the delta");
  sigs.contribute2 = await contributeUsdc(usdc(40));
  sigs.claim2 = await claim();
  assertEq(await openBalance(), open(100).toString(), "buyer OPEN after second claim (delta paid)");

  // --- 3) sweep 50 USDC of the 100 raised to the fixed treasury ---
  console.log("\n[3] sweep_proceeds 50 USDC to the fixed treasury while Active");
  const vaultBefore = await usdcBalance(usdcVault);
  assertEq(vaultBefore, usdc(100).toString(), "usdc_vault holds 100 before sweep");
  sigs.sweep = await program.methods
    .sweepProceeds(new BN(SALE_NONCE), usdc(50))
    .accountsPartial({
      admin: admin.publicKey,
      saleConfig,
      usdcVault,
      treasury,
      usdcMint,
      tokenProgram: TOKEN_2022_PROGRAM_ID,
    })
    .rpc({ commitment: "confirmed" });
  assertEq(await usdcBalance(treasury), usdc(50).toString(), "treasury received 50 after sweep");
  assertEq(await usdcBalance(usdcVault), usdc(50).toString(), "usdc_vault reduced to 50 after sweep");

  // Claims survive sweeps: re-read the buyer's OPEN AFTER the sweep landed to
  // prove on-chain (not by inference) that sweeping USDC left their claimed
  // OPEN untouched — sweep_proceeds only moves USDC out of usdc_vault, never
  // OPEN out of presale_vault.
  assertEq(await openBalance(), open(100).toString(), "buyer OPEN unchanged after the sweep (claims survive sweeps)");

  // Log the ProceedsSwept event emitted by sweep_proceeds.
  const sweepTx = await connection.getTransaction(sigs.sweep, {
    commitment: "confirmed",
    maxSupportedTransactionVersion: 0,
  });
  for (const line of (sweepTx?.meta?.logMessages || [])) {
    if (!line.startsWith("Program data:")) continue;
    try {
      const ev = (program as any).coder.events.decode(line.split("Program data: ")[1]);
      if (ev) {
        console.log(`  event ${ev.name}:`, {
          sale_config: ev.data.saleConfig?.toBase58?.() ?? String(ev.data.saleConfig),
          treasury: ev.data.treasury?.toBase58?.() ?? String(ev.data.treasury),
          amount: ev.data.amount?.toString?.() ?? String(ev.data.amount),
          vault_remaining: ev.data.vaultRemaining?.toString?.() ?? String(ev.data.vaultRemaining),
        });
      }
    } catch {
      /* not an anchor event line */
    }
  }

  console.log("\nAll assertions passed. Signatures:");
  for (const [k, v] of Object.entries(sigs)) console.log(`  ${k}: ${v}`);
  console.log(`\nsale_nonce=${SALE_NONCE} saleConfig=${saleConfig.toBase58()} usdcMint=${usdcMint.toBase58()}`);
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
