/**
 * Draws a fixed amount of OPEN out of the presale vault and hands it to the
 * devnet faucet authority, by running one complete, real presale cycle.
 *
 * WHY THIS EXISTS
 *
 * The devnet faucet (openfiat-faucet) hands testers SOL, mock USDC, mock USDT
 * and OPEN. The first three it can *mint* — it holds their mint authority.
 * OPEN it cannot: the OPEN mint's authority is permanently unset (verified
 * below at runtime, not assumed), so the total supply is fixed forever and the
 * only OPEN that can still move on devnet is the 200,000,000 sitting in the
 * Community Presale bucket. The presale program's only exit from that bucket
 * is `claim`, and `claim` only pays out to a wallet that actually contributed
 * to a sale which actually reached `Finalized`.
 *
 * So there is no shortcut here and this script does not attempt one: it opens
 * a real sale at a fresh nonce, mints itself test-USDC, contributes, finalizes,
 * claims its entitlement, and forwards the OPEN to the faucet authority. Every
 * OPEN the faucet ever dispenses came out of this cycle.
 *
 * This is devnet faucet tooling. It is NOT a tokenomics event: the OPEN moved
 * here is not sold, not allocated, and not owed to anyone. Anyone auditing the
 * presale bucket later and finding a 1,000,000 OPEN outflow should read the
 * `faucet_open_draw` record this script writes into devnet-addresses.json.
 *
 * IDEMPOTENT. Every step checks on-chain state first and skips itself if it
 * has already happened, so a re-run after a crash, an RPC timeout, or a
 * killed terminal resumes rather than double-spending. Re-running after a
 * fully successful run is a no-op.
 *
 * Usage: npx ts-node scripts/extract-open-for-faucet.ts
 */
import * as fs from "fs";
import * as path from "path";
import {
  Connection,
  Keypair,
  PublicKey,
  SYSVAR_RENT_PUBKEY,
  Transaction,
  clusterApiUrl,
  sendAndConfirmTransaction,
} from "@solana/web3.js";
import {
  TOKEN_2022_PROGRAM_ID,
  createAssociatedTokenAccountIdempotentInstruction,
  createTransferCheckedInstruction,
  getAccount,
  getAssociatedTokenAddressSync,
  getMint,
  mintTo,
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

/** The devnet faucet's authority — the wallet openfiat-faucet signs with. */
const FAUCET_AUTHORITY = new PublicKey(
  "4oiCmGrMRL4m4RJsRX6F7nCDeEqoiKLYm5hsDcLFvAJB",
);

/**
 * Nonce 0 is a stale sale against the pre-correction mint. Nonce 1 is the
 * live, tester-facing presale (OFS-4100 §3 terms) and MUST be left alone: it
 * has to stay `Active` until its 2026-08-26 close, and this script calls
 * `finalize_sale`, which is irreversible. Finalizing nonce 1 to claim from it
 * would end the presale for every future tester. Hence a fresh nonce, and the
 * assertion below that makes pointing this script at nonce 1 impossible rather
 * than merely inadvisable.
 */
const SALE_NONCE = 2;

/** Sales that exist for reasons other than faucet tooling, and that this
 *  script must never finalize. */
const PROTECTED_SALE_NONCES = [0, 1];

/** The entire Community Presale bucket, in OPEN. A hard cap above this would
 *  let a sale promise entitlements the vault cannot pay. */
const PRESALE_BUCKET_OPEN = 200_000_000;

/** OPEN to deliver to the faucet. 1 OPEN = 1 USDC, so this is also the USDC
 *  contribution required to earn it. Leaves 199,000,000 in the bucket. */
const TARGET_OPEN = 1_000_000;

const USDC_DECIMALS = 6;
const OPEN_DECIMALS = 9;

/**
 * Sale window, in seconds either side of the *chain's* clock (never the local
 * one — see `chainTime`).
 *
 * Backdating the start guards against the validator's clock running behind
 * this process (`SaleNotStarted`); the generous end guards against it running
 * ahead (`SaleEnded` on the contribution itself, which is the failure that
 * actually wastes a cycle — a sale whose window shut before the contribution
 * landed can never be finalized into a claimable state).
 *
 * A long window costs nothing here because finalization does not depend on it:
 * see `hardCap` in `saleParams`.
 */
const WINDOW_BACKDATE_SECS = 300;
const WINDOW_LENGTH_SECS = 600;

/**
 * The subset of each account this script actually reads. Declared by hand
 * because the IDL is loaded at runtime (see `loadIdl`) rather than through
 * `target/types`, so Anchor's `program.account` namespace is untyped here;
 * these give the reads real types instead of `any`. Field names are Anchor's
 * camelCase rendering of `state.rs`.
 */
interface SaleConfigAccount {
  state: Record<string, unknown>;
  hardCap: BN;
  softCap: BN;
  maxContribution: BN;
  totalRaised: BN;
  endTime: BN;
}
interface ContributionAccount {
  amountUsdc: BN;
  openEntitlement: BN;
  claimed: boolean;
}
interface PresaleAccounts {
  saleConfig: { fetch(address: PublicKey): Promise<SaleConfigAccount> };
  contribution: { fetch(address: PublicKey): Promise<ContributionAccount> };
}

const usdcUnits = (whole: number) => BigInt(whole) * 10n ** BigInt(USDC_DECIMALS);
const openUnits = (whole: number) => BigInt(whole) * 10n ** BigInt(OPEN_DECIMALS);
const bn = (v: bigint) => new BN(v.toString());
const leU64 = (n: number) => new BN(n).toArrayLike(Buffer, "le", 8);

/**
 * The validator's `Clock` sysvar time, which is what every `require!` in the
 * presale program compares against — not `Date.now()`. Devnet's clock drifts
 * from wall clock, and two presale tests in this repo have already been broken
 * by assuming they were the same thing.
 *
 * `getBlockTime` returns null for slots that have been skipped or not yet had
 * their time recorded, so retry on a fresher slot rather than treating null as
 * an error.
 */
async function chainTime(connection: Connection): Promise<number> {
  for (let attempt = 0; attempt < 5; attempt++) {
    const slot = await connection.getSlot("confirmed");
    const time = await connection.getBlockTime(slot);
    if (time !== null) return time;
    await sleep(1_000);
  }
  throw new Error("could not read the validator's block time after 5 attempts");
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Token balance in base units, or 0n if the account doesn't exist yet. */
async function tokenBalance(
  connection: Connection,
  address: PublicKey,
): Promise<bigint> {
  const info = await connection.getAccountInfo(address, "confirmed");
  if (!info) return 0n;
  const account = await getAccount(connection, address, "confirmed", TOKEN_2022_PROGRAM_ID);
  return account.amount;
}

const whole = (units: bigint, decimals: number) =>
  (Number(units) / 10 ** decimals).toLocaleString("en-US");

/**
 * Loads the presale IDL, preferring a local `anchor build` artifact and
 * falling back to the copy the program published on-chain.
 *
 * The on-chain IDL is STALE: it predates the OFS-7100 §12 ban list, so its
 * `contribute_usdc` is missing the `ban_record` account that the deployed
 * *binary* requires. (`anchor deploy` and `anchor idl upgrade` are separate
 * steps and only the first was run.) Encoding against the stale account list
 * shifts every account one slot left, and the program rejects the transaction
 * with `AccountOwnedByWrongProgram` on `sale_config` — it reads `buyer_usdc`
 * where it expected the config. So the account is spliced back in at the index
 * the binary expects. Verified against a from-source `anchor build` IDL and by
 * simulating both shapes against the live program.
 */
async function loadIdl(provider: anchor.AnchorProvider): Promise<anchor.Idl> {
  const localPath = path.join(__dirname, "..", "target", "idl", "presale.json");
  let idl: anchor.Idl;
  if (fs.existsSync(localPath)) {
    idl = JSON.parse(fs.readFileSync(localPath, "utf-8"));
    console.log("IDL: local target/idl/presale.json");
  } else {
    const fetched = await anchor.Program.fetchIdl(PRESALE_PROGRAM_ID, provider);
    if (!fetched) {
      throw new Error(
        "no local target/idl/presale.json and the program publishes no IDL on-chain — run `anchor build`",
      );
    }
    idl = fetched;
    console.log("IDL: fetched from chain (no local build artifact)");
  }

  const contribute = idl.instructions.find((i) => i.name === "contribute_usdc");
  if (!contribute) throw new Error("IDL has no contribute_usdc instruction");
  if (!contribute.accounts.some((a) => a.name === "ban_record")) {
    contribute.accounts.splice(1, 0, {
      name: "ban_record",
      writable: false,
      signer: false,
    } as (typeof contribute.accounts)[number]);
    console.log("IDL: patched stale contribute_usdc — re-inserted ban_record at index 1");
  }
  return idl;
}

/**
 * Sale terms for this cycle.
 *
 * `hardCap` is set to *exactly* the contribution, which is the single most
 * important choice in this script. `finalize_sale` unlocks on
 * `now > end_time || total_raised >= hard_cap`, so hitting the cap on the nose
 * makes finalization reachable the instant the contribution lands, with no
 * dependence on the validator's clock at all. The timed window remains as a
 * second, independent path in case the contribution ever comes up short.
 *
 * `softCap` is ZERO, which is the other important choice. A sale that ends
 * with `total_raised < soft_cap` resolves to `SoftCapMissed`, which permits
 * refunds and forbids `claim` *permanently* — the cycle would be dead and the
 * OPEN unreachable through it. The program only requires
 * `hard_cap > soft_cap`, so zero is legal, and at zero `soft_cap_met` is
 * `total_raised >= 0`, which is unconditionally true. `finalize_sale` can
 * therefore only ever resolve to `Finalized`, and the trap is not merely
 * avoided by a comfortable margin, it is removed.
 *
 * `maxContribution` equals the hard cap because one wallet is meant to take
 * this entire sale; it exists to move OPEN to the faucet, not to distribute.
 * The hard cap must never exceed the 200,000,000 OPEN actually in the bucket,
 * or the sale could promise entitlements it cannot pay out.
 */
function saleParams(chainNow: number) {
  return {
    hardCap: usdcUnits(TARGET_OPEN),
    softCap: 0n,
    minContribution: usdcUnits(1),
    maxContribution: usdcUnits(TARGET_OPEN),
    maxSlippageBps: 100,
    startTime: chainNow - WINDOW_BACKDATE_SECS,
    endTime: chainNow + WINDOW_LENGTH_SECS,
  };
}

async function main() {
  if (PROTECTED_SALE_NONCES.includes(SALE_NONCE)) {
    throw new Error(
      `refusing to run against sale nonce ${SALE_NONCE}: it is a protected sale ` +
        "(nonce 1 is the live tester-facing presale and must stay Active until it " +
        "closes). This script finalizes the sale it uses, which cannot be undone. " +
        "Pick an unused nonce.",
    );
  }
  if (TARGET_OPEN > PRESALE_BUCKET_OPEN) {
    throw new Error(
      `TARGET_OPEN ${TARGET_OPEN} exceeds the ${PRESALE_BUCKET_OPEN} OPEN in the bucket`,
    );
  }

  const connection = new Connection(clusterApiUrl("devnet"), "confirmed");
  const keypairPath =
    process.env.SOLANA_KEYPAIR ||
    path.join(process.env.HOME || "~", ".config/solana/id.json");
  const admin = Keypair.fromSecretKey(
    Uint8Array.from(JSON.parse(fs.readFileSync(keypairPath, "utf-8"))),
  );
  console.log(`Admin: ${admin.publicKey.toBase58()}`);
  console.log(`Faucet authority (destination): ${FAUCET_AUTHORITY.toBase58()}`);
  console.log(`Target: ${TARGET_OPEN.toLocaleString("en-US")} OPEN via sale nonce ${SALE_NONCE}\n`);

  const addrPath = path.join(__dirname, "..", "devnet-addresses.json");
  const allAddresses = JSON.parse(fs.readFileSync(addrPath, "utf-8"));
  const genesis = allAddresses.devnet;
  const priorSale = allAddresses.devnet_sale;
  if (!genesis || !priorSale) {
    throw new Error("devnet-addresses.json is missing the devnet genesis or devnet_sale entry");
  }
  const openMint = new PublicKey(genesis.mint);
  const presaleVault = new PublicKey(genesis["bucket_community-presale"]);
  const usdcMint = new PublicKey(priorSale.usdcMint);

  // ---- Step 0: verify every assumption this cycle rests on ---------------
  // Cheap to check, and each one is a way to burn a cycle or, worse, to move
  // OPEN somewhere unintended. Stop on any surprise rather than adapt to it.
  console.log("=== Step 0: verifying on-chain assumptions ===");

  const open = await getMint(connection, openMint, "confirmed", TOKEN_2022_PROGRAM_ID);
  if (open.mintAuthority !== null) {
    throw new Error(
      `OPEN mint authority is ${open.mintAuthority.toBase58()}, expected unset — ` +
        "if OPEN is mintable again this whole extraction is the wrong approach",
    );
  }
  if (open.decimals !== OPEN_DECIMALS) {
    throw new Error(`OPEN decimals are ${open.decimals}, expected ${OPEN_DECIMALS}`);
  }
  console.log(`OPEN ${openMint.toBase58()}: ${open.decimals} decimals, mint authority unset`);

  const usdc = await getMint(connection, usdcMint, "confirmed", TOKEN_2022_PROGRAM_ID);
  if (!usdc.mintAuthority?.equals(admin.publicKey)) {
    throw new Error(
      `sale USDC mint authority is ${usdc.mintAuthority?.toBase58() ?? "unset"}, ` +
        `expected the local admin ${admin.publicKey.toBase58()} — cannot mint a contribution`,
    );
  }
  if (usdc.decimals !== USDC_DECIMALS) {
    throw new Error(`sale USDC decimals are ${usdc.decimals}, expected ${USDC_DECIMALS}`);
  }
  console.log(`USDC ${usdcMint.toBase58()}: ${usdc.decimals} decimals, mintable by admin`);

  const [presaleVaultAuthority] = PublicKey.findProgramAddressSync(
    [Buffer.from("presale_vault")],
    PRESALE_PROGRAM_ID,
  );
  const vaultAccount = await getAccount(
    connection,
    presaleVault,
    "confirmed",
    TOKEN_2022_PROGRAM_ID,
  );
  if (!vaultAccount.owner.equals(presaleVaultAuthority)) {
    throw new Error(
      `presale vault owner is ${vaultAccount.owner.toBase58()}, expected the ` +
        `presale_vault PDA ${presaleVaultAuthority.toBase58()}`,
    );
  }
  if (!vaultAccount.mint.equals(openMint)) {
    throw new Error(`presale vault holds ${vaultAccount.mint.toBase58()}, not OPEN`);
  }
  const vaultBefore = vaultAccount.amount;
  if (vaultBefore < openUnits(TARGET_OPEN)) {
    throw new Error(
      `presale vault holds only ${whole(vaultBefore, OPEN_DECIMALS)} OPEN, ` +
        `need ${TARGET_OPEN.toLocaleString("en-US")}`,
    );
  }
  console.log(
    `Presale vault ${presaleVault.toBase58()}: ${whole(vaultBefore, OPEN_DECIMALS)} OPEN ` +
      `(authority PDA ${presaleVaultAuthority.toBase58()})`,
  );

  const adminUsdcAta = getAssociatedTokenAddressSync(
    usdcMint, admin.publicKey, false, TOKEN_2022_PROGRAM_ID,
  );
  const adminOpenAta = getAssociatedTokenAddressSync(
    openMint, admin.publicKey, false, TOKEN_2022_PROGRAM_ID,
  );
  const faucetOpenAta = getAssociatedTokenAddressSync(
    openMint, FAUCET_AUTHORITY, false, TOKEN_2022_PROGRAM_ID,
  );
  const faucetOpenBefore = await tokenBalance(connection, faucetOpenAta);
  console.log(`Faucet OPEN ATA ${faucetOpenAta.toBase58()}: ${whole(faucetOpenBefore, OPEN_DECIMALS)} OPEN\n`);

  const signatures: Record<string, string> = {};
  const wallet = new anchor.Wallet(admin);
  const provider = new anchor.AnchorProvider(connection, wallet, { commitment: "confirmed" });
  const program = new anchor.Program(await loadIdl(provider), provider);
  const accounts = program.account as unknown as PresaleAccounts;

  const [saleConfigPda] = PublicKey.findProgramAddressSync(
    [Buffer.from("sale_config"), leU64(SALE_NONCE)], PRESALE_PROGRAM_ID,
  );
  const [usdcVault] = PublicKey.findProgramAddressSync(
    [Buffer.from("sale_usdc_vault"), leU64(SALE_NONCE)], PRESALE_PROGRAM_ID,
  );
  const [contributionPda] = PublicKey.findProgramAddressSync(
    [Buffer.from("contribution"), saleConfigPda.toBytes(), admin.publicKey.toBytes()],
    PRESALE_PROGRAM_ID,
  );
  // Proof-of-non-existence ban gate (OFS-7100 §12): the buyer is banned iff
  // this governance PDA is occupied. Passing it is mandatory even when — as
  // here — it is empty.
  const [banRecord] = PublicKey.findProgramAddressSync(
    [Buffer.from("ban"), admin.publicKey.toBytes()], GOVERNANCE_PROGRAM_ID,
  );

  // ---- Step 1: the sale ---------------------------------------------------
  console.log("=== Step 1: sale config ===");
  if (await connection.getAccountInfo(saleConfigPda)) {
    console.log(`sale_config ${saleConfigPda.toBase58()} already exists — reusing`);
  } else {
    const chainNow = await chainTime(connection);
    const local = Math.floor(Date.now() / 1000);
    console.log(
      `chain clock ${chainNow} vs local ${local} (drift ${chainNow - local}s) — using chain clock`,
    );
    const p = saleParams(chainNow);
    console.log(
      `hardCap ${whole(p.hardCap, USDC_DECIMALS)} / softCap ${whole(p.softCap, USDC_DECIMALS)} ` +
        `USDC, window ${new Date(p.startTime * 1000).toISOString()} → ${new Date(p.endTime * 1000).toISOString()}`,
    );
    const sig = await program.methods
      .initializeSale(new BN(SALE_NONCE), {
        hardCap: bn(p.hardCap),
        softCap: bn(p.softCap),
        minContribution: bn(p.minContribution),
        maxContribution: bn(p.maxContribution),
        maxSlippageBps: p.maxSlippageBps,
        startTime: new BN(p.startTime),
        endTime: new BN(p.endTime),
        stablecoinWhitelist: [],
      })
      .accountsPartial({
        admin: admin.publicKey,
        saleConfig: saleConfigPda,
        openMint,
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
    signatures.initializeSale = sig;
    console.log(`initialize_sale: ${sig}`);
  }

  let sale = await accounts.saleConfig.fetch(saleConfigPda);
  const stateName = (s: object) => Object.keys(s)[0];
  console.log(`state=${stateName(sale.state)} totalRaised=${whole(BigInt(sale.totalRaised.toString()), USDC_DECIMALS)} USDC`);

  // A pre-existing sale at this nonce may have terms that cannot deliver the
  // target. Refuse rather than contribute into a dead end.
  if (BigInt(sale.softCap.toString()) > usdcUnits(TARGET_OPEN)) {
    throw new Error(
      `sale nonce ${SALE_NONCE} has softCap ${whole(BigInt(sale.softCap.toString()), USDC_DECIMALS)} ` +
        `USDC, above the ${TARGET_OPEN} contribution — it would resolve SoftCapMissed and never permit claim`,
    );
  }
  if (BigInt(sale.maxContribution.toString()) < usdcUnits(TARGET_OPEN)) {
    throw new Error(
      `sale nonce ${SALE_NONCE} caps a wallet at ` +
        `${whole(BigInt(sale.maxContribution.toString()), USDC_DECIMALS)} USDC, below the target`,
    );
  }
  if (stateName(sale.state) === "softCapMissed") {
    throw new Error(
      `sale nonce ${SALE_NONCE} resolved SoftCapMissed — claim is permanently forbidden. ` +
        "Bump SALE_NONCE and re-run; do not try to salvage this one.",
    );
  }

  // ---- Step 2: fund the contribution -------------------------------------
  console.log("\n=== Step 2: test-USDC for the contribution ===");
  const contributionInfo = await connection.getAccountInfo(contributionPda);
  const alreadyContributed = contributionInfo
    ? BigInt((await accounts.contribution.fetch(contributionPda)).amountUsdc.toString())
    : 0n;
  const stillToContribute = usdcUnits(TARGET_OPEN) - alreadyContributed;

  if (stillToContribute <= 0n) {
    console.log(`already contributed ${whole(alreadyContributed, USDC_DECIMALS)} USDC — skipping`);
  } else {
    const usdcHeld = await tokenBalance(connection, adminUsdcAta);
    console.log(`admin holds ${whole(usdcHeld, USDC_DECIMALS)} USDC, needs ${whole(stillToContribute, USDC_DECIMALS)}`);
    if (usdcHeld < stillToContribute) {
      const shortfall = stillToContribute - usdcHeld;
      const sig = await mintTo(
        connection, admin, usdcMint, adminUsdcAta, admin, shortfall,
        [], { commitment: "confirmed" }, TOKEN_2022_PROGRAM_ID,
      );
      signatures.mintUsdc = sig;
      console.log(`minted ${whole(shortfall, USDC_DECIMALS)} test-USDC: ${sig}`);
    } else {
      console.log("no minting needed");
    }

    // ---- Step 3: contribute ---------------------------------------------
    console.log("\n=== Step 3: contribute ===");
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
    signatures.contributeUsdc = sig;
    console.log(`contribute_usdc ${whole(stillToContribute, USDC_DECIMALS)} USDC: ${sig}`);
  }

  const contribution = await accounts.contribution.fetch(contributionPda);
  const entitlement = BigInt(contribution.openEntitlement.toString());
  console.log(
    `entitlement: ${whole(entitlement, OPEN_DECIMALS)} OPEN (claimed=${contribution.claimed})`,
  );
  if (entitlement < openUnits(TARGET_OPEN)) {
    throw new Error(
      `entitlement ${whole(entitlement, OPEN_DECIMALS)} OPEN is below the target — ` +
        "the contribution did not fully land",
    );
  }

  // ---- Step 4: finalize --------------------------------------------------
  console.log("\n=== Step 4: finalize ===");
  sale = await accounts.saleConfig.fetch(saleConfigPda);
  if (stateName(sale.state) === "finalized") {
    console.log("sale already Finalized — skipping");
  } else if (stateName(sale.state) === "softCapMissed") {
    throw new Error("sale resolved SoftCapMissed — claim is permanently forbidden");
  } else {
    const endTime = Number(sale.endTime);
    const hardCap = BigInt(sale.hardCap.toString());
    const raised = BigInt(sale.totalRaised.toString());

    // finalize_sale unlocks on `now > end_time || total_raised >= hard_cap`.
    // Poll the *validator's* clock for the first condition; the second is
    // already decided by the contribution above and needs no waiting.
    if (raised >= hardCap) {
      console.log(
        `hard cap reached (${whole(raised, USDC_DECIMALS)} >= ${whole(hardCap, USDC_DECIMALS)} USDC) — ` +
          "finalization already unlocked, no need to wait out the window",
      );
    } else {
      console.log(`waiting for the chain clock to pass end_time ${endTime} (hard cap not reached)`);
      for (;;) {
        const now = await chainTime(connection);
        if (now > endTime) {
          console.log(`chain clock ${now} > end_time ${endTime} — window closed`);
          break;
        }
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
    signatures.finalizeSale = sig;
    console.log(`finalize_sale: ${sig}`);
  }

  sale = await accounts.saleConfig.fetch(saleConfigPda);
  if (stateName(sale.state) !== "finalized") {
    throw new Error(`sale is ${stateName(sale.state)} after finalize, expected Finalized`);
  }
  console.log("state=Finalized — claim is now permitted");

  // ---- Step 5: claim -----------------------------------------------------
  console.log("\n=== Step 5: claim ===");
  if (contribution.claimed) {
    console.log("entitlement already claimed — skipping");
  } else {
    if (!(await connection.getAccountInfo(adminOpenAta))) {
      const tx = new Transaction().add(
        createAssociatedTokenAccountIdempotentInstruction(
          admin.publicKey, adminOpenAta, admin.publicKey, openMint, TOKEN_2022_PROGRAM_ID,
        ),
      );
      const sig = await sendAndConfirmTransaction(connection, tx, [admin], { commitment: "confirmed" });
      signatures.createAdminOpenAta = sig;
      console.log(`created admin OPEN ATA ${adminOpenAta.toBase58()}: ${sig}`);
    }
    const sig = await program.methods
      .claim(new BN(SALE_NONCE))
      .accountsPartial({
        buyer: admin.publicKey,
        saleConfig: saleConfigPda,
        openMint,
        presaleVaultAuthority,
        presaleVault,
        contribution: contributionPda,
        buyerOpen: adminOpenAta,
        tokenProgram: TOKEN_2022_PROGRAM_ID,
      })
      .rpc({ commitment: "confirmed" });
    signatures.claim = sig;
    console.log(`claim: ${sig}`);
  }
  const adminOpen = await tokenBalance(connection, adminOpenAta);
  console.log(`admin now holds ${whole(adminOpen, OPEN_DECIMALS)} OPEN`);

  // ---- Step 6: hand it to the faucet ------------------------------------
  console.log("\n=== Step 6: transfer to the faucet authority ===");
  const faucetShortfall = openUnits(TARGET_OPEN) - faucetOpenBefore;
  if (faucetShortfall <= 0n) {
    console.log(
      `faucet already holds ${whole(faucetOpenBefore, OPEN_DECIMALS)} OPEN — nothing to send`,
    );
  } else {
    const toSend = adminOpen < faucetShortfall ? adminOpen : faucetShortfall;
    if (toSend <= 0n) {
      throw new Error("admin holds no OPEN to forward — claim did not deliver");
    }
    const tx = new Transaction().add(
      // Idempotent: the faucet's OPEN ATA may already exist from an earlier
      // run or from manual setup on the VPS. Its address is deterministic
      // either way, so creating it here is equivalent to creating it there.
      createAssociatedTokenAccountIdempotentInstruction(
        admin.publicKey, faucetOpenAta, FAUCET_AUTHORITY, openMint, TOKEN_2022_PROGRAM_ID,
      ),
      createTransferCheckedInstruction(
        adminOpenAta, openMint, faucetOpenAta, admin.publicKey,
        toSend, OPEN_DECIMALS, [], TOKEN_2022_PROGRAM_ID,
      ),
    );
    const sig = await sendAndConfirmTransaction(connection, tx, [admin], { commitment: "confirmed" });
    signatures.transferToFaucet = sig;
    console.log(`transferred ${whole(toSend, OPEN_DECIMALS)} OPEN to the faucet: ${sig}`);
  }

  // ---- Step 7: verify by reading the chain, not by trusting the sends ----
  console.log("\n=== Step 7: verification (balances read back from chain) ===");
  const faucetOpenAfter = await tokenBalance(connection, faucetOpenAta);
  const vaultAfter = await tokenBalance(connection, presaleVault);
  console.log(`presale vault: ${whole(vaultBefore, OPEN_DECIMALS)} → ${whole(vaultAfter, OPEN_DECIMALS)} OPEN`);
  console.log(`faucet OPEN:   ${whole(faucetOpenBefore, OPEN_DECIMALS)} → ${whole(faucetOpenAfter, OPEN_DECIMALS)} OPEN`);

  if (faucetOpenAfter < openUnits(TARGET_OPEN)) {
    throw new Error(
      `faucet holds ${whole(faucetOpenAfter, OPEN_DECIMALS)} OPEN, expected at least ` +
        `${TARGET_OPEN.toLocaleString("en-US")} — the transfer did not deliver`,
    );
  }
  const drawn = vaultBefore - vaultAfter;
  console.log(`drawn from the vault this cycle: ${whole(drawn, OPEN_DECIMALS)} OPEN`);

  // ---- Step 8: record why the bucket is 1M lighter ----------------------
  // This record IS the audit trail, so a re-run must never destroy it. Being
  // idempotent, a second run performs no transactions and observes a vault
  // that is already drawn down — writing that naively would report
  // before == after and blank out the signatures that prove the draw ever
  // happened. So the first run's observations and every signature seen are
  // carried forward and merged rather than overwritten.
  const prior = (allAddresses.faucet_open_draw ?? {}) as Record<string, unknown>;
  const priorSignatures = (prior.signatures ?? {}) as Record<string, string>;
  allAddresses.faucet_open_draw = {
    reason:
      "Devnet faucet tooling, NOT an accounting anomaly. OPEN's mint authority is " +
      "permanently unset, so the devnet faucet cannot mint OPEN the way it mints mock " +
      "USDC/USDT — the only movable OPEN on devnet is the Community Presale bucket, and " +
      "the only way out of it is claim() on a Finalized sale. This draw is the proceeds " +
      "of one deliberate, fully-executed presale cycle at the nonce below, run purely to " +
      "stock the faucet. Nothing was sold and nothing is owed to anyone. Reproduce or " +
      "audit with scripts/extract-open-for-faucet.ts.",
    saleNonce: SALE_NONCE,
    saleConfig: saleConfigPda.toBase58(),
    openDrawn: TARGET_OPEN,
    usdcContributed: TARGET_OPEN,
    presaleVault: presaleVault.toBase58(),
    presaleVaultOpenBefore:
      prior.presaleVaultOpenBefore ?? Number(vaultBefore) / 10 ** OPEN_DECIMALS,
    presaleVaultOpenAfter: Number(vaultAfter) / 10 ** OPEN_DECIMALS,
    faucetAuthority: FAUCET_AUTHORITY.toBase58(),
    faucetOpenAta: faucetOpenAta.toBase58(),
    faucetOpenBalance: Number(faucetOpenAfter) / 10 ** OPEN_DECIMALS,
    signatures: { ...priorSignatures, ...signatures },
    executedAt: prior.executedAt ?? new Date().toISOString(),
    lastVerifiedAt: new Date().toISOString(),
  };
  fs.writeFileSync(addrPath, JSON.stringify(allAddresses, null, 2) + "\n");
  console.log("\nRecorded faucet_open_draw in devnet-addresses.json");

  console.log("\n=== Signatures ===");
  for (const [step, sig] of Object.entries(signatures)) console.log(`${step}: ${sig}`);
  if (Object.keys(signatures).length === 0) {
    console.log("(none — everything was already done on a previous run)");
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
