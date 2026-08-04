/**
 * Draws a small OPEN tranche into a wallet this machine actually holds the
 * key for, by running one complete, real presale cycle.
 *
 * # Why this exists, when `extract-open-for-faucet.ts` already runs cycles
 *
 * That script runs the same cycle but *forwards* everything it claims to the
 * faucet authority (`4oiCmGrM…`), whose keypair is deliberately not on this
 * host. Its output is therefore unusable for anything that has to *sign* —
 * which is exactly what proving the governance path requires: a real
 * `StakeAccount` can only be created by the wallet that owns the stake, and a
 * vote's weight is only meaningful if it is backed by one.
 *
 * So this is not a second faucet draw and must not be read as one. It is the
 * minimum OPEN needed to stand up a staked identity and exercise
 * stake-weighted voting for real, kept in the admin wallet rather than sent on.
 *
 * # Why a whole presale cycle for 20,000 tokens
 *
 * There is no shortcut and this script does not attempt one. The OPEN mint's
 * authority is permanently unset (verified below at runtime, not assumed), so
 * no new OPEN can ever exist; the only OPEN still able to move on devnet is
 * the Community Presale bucket, and the presale program's sole exit from that
 * bucket is `claim`, which pays only a wallet that genuinely contributed to a
 * sale that genuinely reached `Finalized`.
 *
 * # The sale this uses
 *
 * A fresh nonce, never the live one. Nonce 1 is the tester-facing presale and
 * must stay `Active` until it closes on 2026-08-26; `finalize_sale` is
 * irreversible, so finalizing it to claim from it would end the presale for
 * every future tester. `PROTECTED_SALE_NONCES` makes that impossible rather
 * than merely inadvisable.
 *
 * IDEMPOTENT. Every step checks on-chain state first and skips itself if it
 * has already happened, so a re-run after a crash or an RPC timeout resumes
 * rather than double-spending.
 *
 * Usage:
 *   npx ts-node scripts/obtain-open-for-protocol-proof.ts [--commit]
 *
 * Without `--commit` it reports what it would do and changes nothing.
 */
import * as fs from "fs";
import * as path from "path";
import {
  Connection,
  Keypair,
  PublicKey,
  SYSVAR_RENT_PUBKEY,
  Transaction,
  sendAndConfirmTransaction,
} from "@solana/web3.js";
import {
  TOKEN_2022_PROGRAM_ID,
  createAssociatedTokenAccountIdempotentInstruction,
  createMintToInstruction,
  getAccount,
  getAssociatedTokenAddressSync,
  getMint,
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
const OPEN_MINT = new PublicKey(
  "29w8TroBTYoaqrXBDcpv5L54VZRA8Kf7kU5U1cakvFdj",
);

/** Sales that exist for reasons other than test tooling. Never finalize these. */
const PROTECTED_SALE_NONCES = [0, 1, 2, 3];

const SALE_NONCE = 4;

/**
 * Enough to stake every role's minimum several times over — the live
 * `StakingConfig` floors are 500 (Merchant/Arbitrator), 1,000 (NodeOperator,
 * Oracle, RiskIntelligence, Snapshot) and 5,000 (NotificationProvider) OPEN —
 * with headroom left to make vote weight visibly non-trivial. Deliberately
 * small: this is a protocol proof, not an allocation.
 */
const OPEN_AMOUNT = 20_000;

const USDC_DECIMALS = 6;
const OPEN_DECIMALS = 9;

/** See `extract-open-for-faucet.ts`: backdated start and generous end guard
 *  against the validator's clock drifting either way from this process's. */
const WINDOW_BACKDATE_SECS = 300;
const WINDOW_LENGTH_SECS = 600;

interface SaleConfigAccount {
  state: Record<string, unknown>;
  hardCap: BN;
  totalRaised: BN;
  endTime: BN;
  usdcMint: PublicKey;
  presaleVault: PublicKey;
}
interface ContributionAccount {
  amountUsdc: BN;
  openEntitlement: BN;
  claimed: boolean;
}

const usdcUnits = (v: number) => BigInt(v) * 10n ** BigInt(USDC_DECIMALS);
const openUnits = (v: number) => BigInt(v) * 10n ** BigInt(OPEN_DECIMALS);
const bn = (v: bigint) => new BN(v.toString());
const leU64 = (n: number) => new BN(n).toArrayLike(Buffer, "le", 8);
const stateName = (s: Record<string, unknown>) => Object.keys(s)[0];
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const whole = (units: bigint, decimals: number) =>
  (Number(units) / 10 ** decimals).toLocaleString("en-US");

/** The validator's `Clock` time — what every `require!` compares against.
 *  Never `Date.now()`; devnet's clock drifts from wall clock. */
async function chainTime(connection: Connection): Promise<number> {
  for (let attempt = 0; attempt < 5; attempt++) {
    const slot = await connection.getSlot("confirmed");
    const time = await connection.getBlockTime(slot);
    if (time !== null) return time;
    await sleep(1_000);
  }
  throw new Error("could not read the validator's block time after 5 attempts");
}

async function tokenBalance(
  connection: Connection,
  address: PublicKey,
): Promise<bigint> {
  const info = await connection.getAccountInfo(address, "confirmed");
  if (!info) return 0n;
  const account = await getAccount(
    connection,
    address,
    "confirmed",
    TOKEN_2022_PROGRAM_ID,
  );
  return account.amount;
}

/**
 * Loads the presale IDL, preferring a local `anchor build` artifact.
 *
 * The on-chain published IDL is stale: its `contribute_usdc` is missing the
 * `ban_record` account the deployed binary requires, which shifts every
 * account one slot left and makes the program reject the transaction with
 * `AccountOwnedByWrongProgram`. Patched back in at the index the binary
 * expects — the same fix, for the same reason, as in
 * `extract-open-for-faucet.ts`.
 */
async function loadIdl(provider: anchor.AnchorProvider): Promise<anchor.Idl> {
  const localPath = path.join(__dirname, "..", "target", "idl", "presale.json");
  let idl: anchor.Idl;
  if (fs.existsSync(localPath)) {
    idl = JSON.parse(fs.readFileSync(localPath, "utf-8"));
    console.log("IDL: local target/idl/presale.json");
  } else {
    const fetched = await anchor.Program.fetchIdl(PRESALE_PROGRAM_ID, provider);
    if (!fetched) throw new Error("no local IDL and none published on-chain");
    idl = fetched;
    console.log("IDL: fetched from chain");
  }
  const contribute = idl.instructions.find((i) => i.name === "contribute_usdc");
  if (!contribute) throw new Error("IDL has no contribute_usdc instruction");
  if (!contribute.accounts.some((a) => a.name === "ban_record")) {
    contribute.accounts.splice(1, 0, {
      name: "ban_record",
      writable: false,
      signer: false,
    } as (typeof contribute.accounts)[number]);
    console.log("IDL: patched stale contribute_usdc — re-inserted ban_record");
  }
  return idl;
}

async function main() {
  const commit = process.argv.includes("--commit");
  if (PROTECTED_SALE_NONCES.includes(SALE_NONCE)) {
    throw new Error(
      `refusing to run against sale nonce ${SALE_NONCE}: it is protected. ` +
        "This script finalizes the sale it uses, which cannot be undone.",
    );
  }

  const rpc = process.env.ANCHOR_PROVIDER_URL ?? "https://api.devnet.solana.com";
  if (rpc.includes("mainnet")) {
    throw new Error("devnet-only script; refusing a mainnet endpoint");
  }
  const keypairPath =
    process.env.SOLANA_KEYPAIR ||
    path.join(process.env.HOME || "~", ".config/solana/id.json");
  const admin = Keypair.fromSecretKey(
    Uint8Array.from(JSON.parse(fs.readFileSync(keypairPath, "utf-8"))),
  );
  const connection = new Connection(rpc, "confirmed");
  const provider = new anchor.AnchorProvider(
    connection,
    new anchor.Wallet(admin),
    { commitment: "confirmed" },
  );
  const program = new anchor.Program(await loadIdl(provider), provider);
  const accounts = program.account as unknown as {
    saleConfig: { fetch(a: PublicKey): Promise<SaleConfigAccount> };
    contribution: { fetch(a: PublicKey): Promise<ContributionAccount> };
  };

  console.log(`rpc    : ${rpc}`);
  console.log(`admin  : ${admin.publicKey.toBase58()}`);
  console.log(`nonce  : ${SALE_NONCE}`);
  console.log(`target : ${OPEN_AMOUNT.toLocaleString("en-US")} OPEN`);
  console.log(commit ? "mode   : COMMIT\n" : "mode   : DRY RUN (pass --commit)\n");

  // The premise the whole script rests on: no new OPEN can be created, so a
  // real cycle is the only way to move any. Verified, never assumed.
  const openMintInfo = await getMint(
    connection,
    OPEN_MINT,
    "confirmed",
    TOKEN_2022_PROGRAM_ID,
  );
  if (openMintInfo.mintAuthority !== null) {
    throw new Error(
      `OPEN mint authority is ${openMintInfo.mintAuthority.toBase58()}, expected unset — ` +
        "this script's premise no longer holds; re-check the mint before drawing",
    );
  }
  console.log("OPEN mint authority: unset (fixed supply) — confirmed\n");

  const [saleConfigPda] = PublicKey.findProgramAddressSync(
    [Buffer.from("sale_config"), leU64(SALE_NONCE)],
    PRESALE_PROGRAM_ID,
  );
  const [usdcVault] = PublicKey.findProgramAddressSync(
    [Buffer.from("sale_usdc_vault"), leU64(SALE_NONCE)],
    PRESALE_PROGRAM_ID,
  );
  const [contributionPda] = PublicKey.findProgramAddressSync(
    [
      Buffer.from("contribution"),
      saleConfigPda.toBuffer(),
      admin.publicKey.toBuffer(),
    ],
    PRESALE_PROGRAM_ID,
  );
  const [presaleVaultAuthority] = PublicKey.findProgramAddressSync(
    [Buffer.from("presale_vault")],
    PRESALE_PROGRAM_ID,
  );
  const [banRecord] = PublicKey.findProgramAddressSync(
    [Buffer.from("ban"), admin.publicKey.toBuffer()],
    GOVERNANCE_PROGRAM_ID,
  );

  // The USDC mint and presale vault are read off an existing sale rather than
  // hardcoded, so this script cannot drift from what the live deployment uses.
  const [liveSalePda] = PublicKey.findProgramAddressSync(
    [Buffer.from("sale_config"), leU64(1)],
    PRESALE_PROGRAM_ID,
  );
  const liveSale = await accounts.saleConfig.fetch(liveSalePda);
  const usdcMint = new PublicKey(liveSale.usdcMint);
  const presaleVault = new PublicKey(liveSale.presaleVault);
  console.log(`usdc mint     : ${usdcMint.toBase58()}`);
  console.log(`presale vault : ${presaleVault.toBase58()}`);

  const adminUsdcAta = getAssociatedTokenAddressSync(
    usdcMint,
    admin.publicKey,
    false,
    TOKEN_2022_PROGRAM_ID,
  );
  const adminOpenAta = getAssociatedTokenAddressSync(
    OPEN_MINT,
    admin.publicKey,
    false,
    TOKEN_2022_PROGRAM_ID,
  );
  console.log(`admin USDC ATA: ${adminUsdcAta.toBase58()}`);
  console.log(`admin OPEN ATA: ${adminOpenAta.toBase58()}`);

  const openBefore = await tokenBalance(connection, adminOpenAta);
  console.log(`\nadmin OPEN before: ${whole(openBefore, OPEN_DECIMALS)}`);
  if (openBefore >= openUnits(OPEN_AMOUNT)) {
    console.log("already holds the target — nothing to do.");
    return;
  }
  if (!commit) {
    console.log(
      `\nDRY RUN: would open sale ${SALE_NONCE}, mint ${OPEN_AMOUNT.toLocaleString("en-US")} ` +
        "test USDC, contribute, finalize and claim. Re-run with --commit.",
    );
    return;
  }

  // ---- Step 1: open the sale ---------------------------------------------
  console.log("\n--- Step 1: initialize_sale ---");
  if (await connection.getAccountInfo(saleConfigPda)) {
    console.log(`sale ${SALE_NONCE} already exists — skipping`);
  } else {
    const now = await chainTime(connection);
    const sig = await program.methods
      .initializeSale(new BN(SALE_NONCE), {
        hardCap: bn(usdcUnits(OPEN_AMOUNT)),
        softCap: bn(0n),
        minContribution: bn(usdcUnits(1)),
        maxContribution: bn(usdcUnits(OPEN_AMOUNT)),
        maxSlippageBps: 100,
        startTime: bn(BigInt(now - WINDOW_BACKDATE_SECS)),
        endTime: bn(BigInt(now + WINDOW_LENGTH_SECS)),
        stablecoinWhitelist: [usdcMint],
      })
      .accountsPartial({
        admin: admin.publicKey,
        saleConfig: saleConfigPda,
        openMint: OPEN_MINT,
        usdcMint,
        presaleVaultAuthority,
        presaleVault,
        usdcVault,
        treasury: adminUsdcAta,
        swapProgram: JUPITER_V6_PROGRAM_ID,
        tokenProgram: TOKEN_2022_PROGRAM_ID,
        systemProgram: anchor.web3.SystemProgram.programId,
        rent: SYSVAR_RENT_PUBKEY,
      })
      .rpc({ commitment: "confirmed" });
    console.log(`initialize_sale: ${sig}`);
  }

  // ---- Step 2: mint ourselves the test USDC ------------------------------
  console.log("\n--- Step 2: mint test USDC ---");
  const needUsdc = usdcUnits(OPEN_AMOUNT);
  let usdcBal = await tokenBalance(connection, adminUsdcAta);
  if (usdcBal >= needUsdc) {
    console.log(`already holds ${whole(usdcBal, USDC_DECIMALS)} USDC — skipping`);
  } else {
    const shortfall = needUsdc - usdcBal;
    const tx = new Transaction()
      .add(
        createAssociatedTokenAccountIdempotentInstruction(
          admin.publicKey,
          adminUsdcAta,
          admin.publicKey,
          usdcMint,
          TOKEN_2022_PROGRAM_ID,
        ),
      )
      .add(
        createMintToInstruction(
          usdcMint,
          adminUsdcAta,
          admin.publicKey,
          shortfall,
          [],
          TOKEN_2022_PROGRAM_ID,
        ),
      );
    const sig = await sendAndConfirmTransaction(connection, tx, [admin], {
      commitment: "confirmed",
    });
    console.log(`minted ${whole(shortfall, USDC_DECIMALS)} test USDC: ${sig}`);
    usdcBal = await tokenBalance(connection, adminUsdcAta);
  }

  // ---- Step 3: contribute ------------------------------------------------
  console.log("\n--- Step 3: contribute_usdc ---");
  let sale = await accounts.saleConfig.fetch(saleConfigPda);
  const raised = BigInt(sale.totalRaised.toString());
  const stillToContribute = usdcUnits(OPEN_AMOUNT) - raised;
  if (stillToContribute <= 0n) {
    console.log("hard cap already raised — skipping");
  } else {
    const sig = await program.methods
      .contributeUsdc(new BN(SALE_NONCE), bn(stillToContribute))
      .accountsPartial({
        buyer: admin.publicKey,
        banRecord,
        saleConfig: saleConfigPda,
        buyerUsdc: adminUsdcAta,
        usdcVault,
        usdcMint,
        contribution: contributionPda,
        tokenProgram: TOKEN_2022_PROGRAM_ID,
        systemProgram: anchor.web3.SystemProgram.programId,
      })
      .rpc({ commitment: "confirmed" });
    console.log(
      `contributed ${whole(stillToContribute, USDC_DECIMALS)} USDC: ${sig}`,
    );
  }

  const contribution = await accounts.contribution.fetch(contributionPda);
  const entitlement = BigInt(contribution.openEntitlement.toString());
  console.log(
    `entitlement: ${whole(entitlement, OPEN_DECIMALS)} OPEN (claimed=${contribution.claimed})`,
  );

  // ---- Step 4: finalize --------------------------------------------------
  console.log("\n--- Step 4: finalize_sale ---");
  sale = await accounts.saleConfig.fetch(saleConfigPda);
  if (stateName(sale.state) === "finalized") {
    console.log("already Finalized — skipping");
  } else if (stateName(sale.state) === "softCapMissed") {
    throw new Error("sale resolved SoftCapMissed — claim is permanently forbidden");
  } else {
    // Unlocks on `now > end_time || total_raised >= hard_cap`. The cap was set
    // to exactly this contribution, so the second condition is already met and
    // there is nothing to wait for.
    const hardCap = BigInt(sale.hardCap.toString());
    const nowRaised = BigInt(sale.totalRaised.toString());
    if (nowRaised >= hardCap) {
      console.log(
        `hard cap reached (${whole(nowRaised, USDC_DECIMALS)} >= ` +
          `${whole(hardCap, USDC_DECIMALS)} USDC) — finalization unlocked`,
      );
    } else {
      const endTime = Number(sale.endTime);
      console.log(`waiting for chain clock to pass end_time ${endTime}`);
      for (;;) {
        const now = await chainTime(connection);
        if (now > endTime) break;
        console.log(`  chain clock ${now}, ${endTime - now + 1}s to go`);
        await sleep(10_000);
      }
    }
    const sig = await program.methods
      .finalizeSale(new BN(SALE_NONCE))
      .accountsPartial({
        admin: admin.publicKey,
        saleConfig: saleConfigPda,
        usdcVault,
        treasury: adminUsdcAta,
        usdcMint,
        tokenProgram: TOKEN_2022_PROGRAM_ID,
      })
      .rpc({ commitment: "confirmed" });
    console.log(`finalize_sale: ${sig}`);
  }

  sale = await accounts.saleConfig.fetch(saleConfigPda);
  if (stateName(sale.state) !== "finalized") {
    throw new Error(`sale is ${stateName(sale.state)}, expected Finalized`);
  }

  // ---- Step 5: claim -----------------------------------------------------
  console.log("\n--- Step 5: claim ---");
  const fresh = await accounts.contribution.fetch(contributionPda);
  if (fresh.claimed) {
    console.log("already claimed — skipping");
  } else {
    if (!(await connection.getAccountInfo(adminOpenAta))) {
      const tx = new Transaction().add(
        createAssociatedTokenAccountIdempotentInstruction(
          admin.publicKey,
          adminOpenAta,
          admin.publicKey,
          OPEN_MINT,
          TOKEN_2022_PROGRAM_ID,
        ),
      );
      const sig = await sendAndConfirmTransaction(connection, tx, [admin], {
        commitment: "confirmed",
      });
      console.log(`created admin OPEN ATA: ${sig}`);
    }
    const sig = await program.methods
      .claim(new BN(SALE_NONCE))
      .accountsPartial({
        buyer: admin.publicKey,
        saleConfig: saleConfigPda,
        openMint: OPEN_MINT,
        presaleVaultAuthority,
        presaleVault,
        contribution: contributionPda,
        buyerOpen: adminOpenAta,
        tokenProgram: TOKEN_2022_PROGRAM_ID,
      })
      .rpc({ commitment: "confirmed" });
    console.log(`claim: ${sig}`);
  }

  // Read the balance back. A confirmed claim proves the transaction landed,
  // not that the tokens arrived where they were supposed to.
  const openAfter = await tokenBalance(connection, adminOpenAta);
  console.log(`\nadmin OPEN after: ${whole(openAfter, OPEN_DECIMALS)}`);
  if (openAfter - openBefore < entitlement) {
    throw new Error(
      `balance rose by ${whole(openAfter - openBefore, OPEN_DECIMALS)} OPEN, ` +
        `expected at least ${whole(entitlement, OPEN_DECIMALS)}`,
    );
  }
  const vaultLeft = await tokenBalance(connection, presaleVault);
  console.log(`presale bucket remaining: ${whole(vaultLeft, OPEN_DECIMALS)} OPEN`);
  console.log("\ndone.");
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
