/**
 * Draws OPEN out of the presale vault and hands it to the devnet faucet
 * authority, by running complete, real presale cycles.
 *
 * WHY THIS EXISTS
 *
 * The devnet faucet (openfiat-faucet) hands testers SOL, mock USDC, mock USDT
 * and OPEN. The first three it can *mint* — it holds their mint authority.
 * OPEN it cannot: the OPEN mint's authority is permanently unset (verified
 * below at runtime, not assumed), so the total supply is fixed forever and the
 * only OPEN that can still move on devnet is the ~20,000,000,000 sitting in the
 * Community Presale bucket. The presale program's only exit from that bucket
 * is `claim`, and `claim` only pays out to a wallet that actually contributed
 * to a sale which actually reached `Finalized`. That the bucket's owner really
 * is the program's `presale_vault` PDA — and so that no key can move it
 * directly — is checked at Step 0 rather than believed.
 *
 * So there is no shortcut here and this script does not attempt one: for each
 * draw it opens a real sale at a fresh nonce, mints itself test-USDC,
 * contributes, finalizes, claims its entitlement, and forwards the OPEN to the
 * faucet authority. Every OPEN the faucet ever dispenses came out of these
 * cycles.
 *
 * This is devnet faucet tooling. It is NOT a tokenomics event: the OPEN moved
 * here is not sold, not allocated, and not owed to anyone. Anyone auditing the
 * presale bucket later and finding a 600,000,000 OPEN outflow should read the
 * `faucet_open_draw` record this script writes into devnet-addresses.json.
 *
 * IDEMPOTENT. Every step of every draw checks on-chain state first and skips
 * itself if it has already happened, so a re-run after a crash, an RPC
 * timeout, or a killed terminal resumes rather than double-spending. Re-running
 * after a fully successful run is a no-op, verified by doing it.
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

/* Both ids are the re-baseline deployment's (2026-08-09), not the originals.
   The governance id is not free to choose: `contribute_usdc` derives its
   `ban_record` under `openfiat_programs_shared::GOVERNANCE_PROGRAM_ID`, which
   is compiled into the deployed presale binary, so this must be the id that
   binary was built against or every contribution fails on `ConstraintSeeds`. */
const PRESALE_PROGRAM_ID = new PublicKey(
  "7KaEpDzZuqye1xqqp3RnvBJXnDxbU3W9zVrUr5vBS2fU",
);
const GOVERNANCE_PROGRAM_ID = new PublicKey(
  "2k71DBDoxM4SUFYGbyMXFiTSUynPuY2CqFUsx3FuarXF",
);
const JUPITER_V6_PROGRAM_ID = new PublicKey(
  "JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4",
);

/** The devnet faucet's authority — the wallet openfiat-faucet signs with. */
const FAUCET_AUTHORITY = new PublicKey(
  "4oiCmGrMRL4m4RJsRX6F7nCDeEqoiKLYm5hsDcLFvAJB",
);

/**
 * The draws that make up the faucet's stash, each on its own sale nonce.
 *
 * This is a list rather than a single amount because the target grew after the
 * first cycle had already run, and a completed presale cycle is not
 * re-openable: `finalize_sale` and `claim` are both one-way, and a sale's
 * `hard_cap` caps what that sale can ever raise. Topping up therefore means a
 * *new* sale at a *new* nonce, not an amendment to the old one. Keeping both
 * draws listed here — rather than editing 1,000,000 into 10,000,000 and losing
 * the history — is what makes the vault's outflow reconstructable from this
 * file alone.
 *
 * Nonce 0 is a stale sale against the pre-correction mint. Nonce 1 is the live,
 * tester-facing presale and MUST be left alone: it has to stay `Active` until
 * its 2026-09-09 close, and this script calls `finalize_sale`, which is
 * irreversible. Finalizing nonce 1 to claim from it would end the presale for
 * every future tester. Hence fresh nonces, and `PROTECTED_SALE_NONCES` below,
 * which makes pointing this script at nonce 1 impossible rather than merely
 * inadvisable.
 *
 * RESET 2026-08-12 for the re-baseline. The previous entries (nonce 2 for
 * 1,000,000 OPEN and nonce 3 for 9,000,000) were draws against the RETIRED
 * deployment — presale program 75rJ9…, the 9-decimal mint 29w8Tro…, and a
 * different `presale_vault` PDA. They are not history this list can carry
 * forward: nonces are per-program, so leaving them here would not describe
 * past outflows, it would instruct this script to open two brand-new sales on
 * 7KaEp… and draw 10,000,000 OPEN nobody asked for. The old draws remain
 * reconstructable from the retired program's own chain history and from the
 * `faucet_open_draw` record in devnet-addresses.json.
 */
const DRAWS: { nonce: number; openAmount: number }[] = [
  { nonce: 2, openAmount: 600_000_000 },
];

/** Sales that exist for reasons other than faucet tooling, and that this
 *  script must never finalize. */
const PROTECTED_SALE_NONCES = [0, 1];

/** The entire Community Presale bucket, in OPEN. A hard cap above this would
 *  let a sale promise entitlements the vault cannot pay. 20,000,000,000 under
 *  the re-baselined 100B supply, up from 200,000,000 under the old 1B. */
const PRESALE_BUCKET_OPEN = 20_000_000_000;

/**
 * Total OPEN to deliver to the faucet across all draws.
 *
 * 600,000,000 is 599 grants at the faucet's 1,000,500 OPEN drip. It leaves
 * ~19.4B in the bucket, and it is not an irreversible commitment: the
 * `presale_vault` PDA is a singleton shared by every sale, so a later top-up is
 * always one more cycle away.
 */
const TOTAL_TARGET_OPEN = DRAWS.reduce((sum, d) => sum + d.openAmount, 0);

const USDC_DECIMALS = 6;
/* 6, not 9 — the re-genesis mint. Step 0 asserts this against the chain, so a
   stale value here stops the script rather than mis-sizing a draw. */
const OPEN_DECIMALS = 6;

/**
 * OPEN credited per USDC. NOT 1:1 any more, which is the trap in this file:
 * the previous version set every cap and contribution to `usdcUnits(openAmount)`
 * on the assumption that a wallet contributing N USDC receives N OPEN. Under
 * the re-baselined rate a contribution of N USDC receives 100N OPEN, so that
 * arithmetic would size a sale to draw one hundred times the intended amount —
 * and it would succeed, because the bucket is large enough to pay it.
 *
 * Must match the `openPerUsdc` the sale is initialized with; `saleParams`
 * passes this same constant, so the two cannot drift.
 */
const OPEN_PER_USDC = 100;

/** USDC needed to earn `openAmount` OPEN at the configured rate. Exact by
 *  construction: every draw is a whole multiple of OPEN_PER_USDC. */
function usdcFor(openAmount: number): number {
  if (openAmount % OPEN_PER_USDC !== 0) {
    throw new Error(
      `draw of ${openAmount} OPEN is not a whole multiple of the ${OPEN_PER_USDC} ` +
        "OPEN-per-USDC rate, so it cannot be bought exactly",
    );
  }
  return openAmount / OPEN_PER_USDC;
}

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
  /** Optional because a sale predating the re-baseline has no such field;
   *  the rate check treats its absence as a mismatch rather than assuming. */
  openPerUsdc?: BN;
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
const stateName = (s: Record<string, unknown>) => Object.keys(s)[0];

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
 * The published IDL is STALE and must not be treated as a description of what
 * is deployed. Two ways it lies, both found the hard way:
 *
 *  1. Its `contribute_usdc` is missing the `ban_record` account that the
 *     deployed *binary* requires — the IDL predates the OFS-7100 §12 ban list,
 *     and `anchor deploy` and `anchor idl upgrade` are separate steps of which
 *     only the first was run. Encoding against the stale account list shifts
 *     every account one slot left and the program rejects the transaction with
 *     `AccountOwnedByWrongProgram` on `sale_config`, because it reads
 *     `buyer_usdc` where it expected the config. So the account is spliced back
 *     in at the index the binary expects, verified against a from-source
 *     `anchor build` IDL and by simulating both shapes against the live program.
 *  2. It omits `update_sale_params` entirely, even though that instruction is
 *     deployed and callable.
 *
 * Note also that grepping the dumped `.so` for a discriminator does NOT
 * establish whether an instruction exists — SBF does not store them as
 * contiguous literals, and known-present instructions come back absent.
 * Simulation is the only reliable test.
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
 * Sale terms for one draw.
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
 * The hard cap must never exceed the OPEN actually in the bucket, or the sale
 * could promise entitlements it cannot pay out.
 */
function saleParams(chainNow: number, openAmount: number) {
  return {
    hardCap: usdcUnits(usdcFor(openAmount)),
    softCap: 0n,
    minContribution: usdcUnits(1),
    maxContribution: usdcUnits(usdcFor(openAmount)),
    maxSlippageBps: 100,
    openPerUsdc: OPEN_PER_USDC,
    startTime: chainNow - WINDOW_BACKDATE_SECS,
    endTime: chainNow + WINDOW_LENGTH_SECS,
  };
}

interface Context {
  connection: Connection;
  admin: Keypair;
  program: anchor.Program;
  accounts: PresaleAccounts;
  openMint: PublicKey;
  usdcMint: PublicKey;
  presaleVault: PublicKey;
  presaleVaultAuthority: PublicKey;
  adminUsdcAta: PublicKey;
  adminOpenAta: PublicKey;
  banRecord: PublicKey;
}

/**
 * Runs one draw to completion: sale, contribution, finalize, claim. Leaves the
 * claimed OPEN in the admin's own OPEN ATA; forwarding to the faucet happens
 * once, afterwards, for all draws together.
 *
 * Returns the signatures it produced — empty if this draw had already been
 * completed by an earlier run.
 */
async function runDrawCycle(
  ctx: Context,
  draw: { nonce: number; openAmount: number },
): Promise<Record<string, string>> {
  const { connection, admin, program, accounts } = ctx;
  const { nonce, openAmount } = draw;
  const signatures: Record<string, string> = {};

  console.log(`\n########## Draw: ${openAmount.toLocaleString("en-US")} OPEN at nonce ${nonce} ##########`);

  const [saleConfigPda] = PublicKey.findProgramAddressSync(
    [Buffer.from("sale_config"), leU64(nonce)], PRESALE_PROGRAM_ID,
  );
  const [usdcVault] = PublicKey.findProgramAddressSync(
    [Buffer.from("sale_usdc_vault"), leU64(nonce)], PRESALE_PROGRAM_ID,
  );
  const [contributionPda] = PublicKey.findProgramAddressSync(
    [Buffer.from("contribution"), saleConfigPda.toBytes(), admin.publicKey.toBytes()],
    PRESALE_PROGRAM_ID,
  );

  // ---- Step 1: the sale ---------------------------------------------------
  console.log("--- Step 1: sale config ---");
  if (await connection.getAccountInfo(saleConfigPda)) {
    console.log(`sale_config ${saleConfigPda.toBase58()} already exists — reusing`);
  } else {
    const chainNow = await chainTime(connection);
    const local = Math.floor(Date.now() / 1000);
    console.log(
      `chain clock ${chainNow} vs local ${local} (drift ${chainNow - local}s) — using chain clock`,
    );
    const p = saleParams(chainNow, openAmount);
    console.log(
      `hardCap ${whole(p.hardCap, USDC_DECIMALS)} / softCap ${whole(p.softCap, USDC_DECIMALS)} ` +
        `USDC, window ${new Date(p.startTime * 1000).toISOString()} → ${new Date(p.endTime * 1000).toISOString()}`,
    );
    const sig = await program.methods
      .initializeSale(new BN(nonce), {
        hardCap: bn(p.hardCap),
        softCap: bn(p.softCap),
        minContribution: bn(p.minContribution),
        maxContribution: bn(p.maxContribution),
        maxSlippageBps: p.maxSlippageBps,
        /* Omitting this does not fail to compile and does not fail to encode —
           Anchor serializes the absent u64 as zero, and the only thing that
           catches it is the program's own `open_per_usdc > 0` check. Every
           field of `saleParams` has to be spelled out here. */
        openPerUsdc: new BN(p.openPerUsdc),
        startTime: new BN(p.startTime),
        endTime: new BN(p.endTime),
        stablecoinWhitelist: [],
      })
      .accountsPartial({
        admin: admin.publicKey,
        saleConfig: saleConfigPda,
        openMint: ctx.openMint,
        usdcMint: ctx.usdcMint,
        presaleVaultAuthority: ctx.presaleVaultAuthority,
        presaleVault: ctx.presaleVault,
        usdcVault,
        treasury: ctx.adminUsdcAta,
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
  console.log(
    `state=${stateName(sale.state)} totalRaised=${whole(BigInt(sale.totalRaised.toString()), USDC_DECIMALS)} USDC`,
  );

  // A pre-existing sale at this nonce may have terms that cannot deliver this
  // draw. Refuse rather than contribute into a dead end.
  if (BigInt(sale.softCap.toString()) > usdcUnits(usdcFor(openAmount))) {
    throw new Error(
      `sale nonce ${nonce} has softCap ${whole(BigInt(sale.softCap.toString()), USDC_DECIMALS)} ` +
        `USDC, above the ${usdcFor(openAmount)} USDC contribution — it would resolve ` +
        "SoftCapMissed and never permit claim",
    );
  }
  /* A pre-existing sale's rate matters as much as its caps: the contribution
     below is sized from OPEN_PER_USDC, so a sale initialized at a different
     rate would under- or over-deliver against this draw. */
  const saleRate = BigInt(sale.openPerUsdc?.toString() ?? "0");
  if (saleRate !== BigInt(OPEN_PER_USDC)) {
    throw new Error(
      `sale nonce ${nonce} credits ${saleRate} OPEN per USDC, not the ${OPEN_PER_USDC} ` +
        "this draw is sized for — use a fresh nonce rather than contributing into it",
    );
  }
  if (BigInt(sale.maxContribution.toString()) < usdcUnits(usdcFor(openAmount))) {
    throw new Error(
      `sale nonce ${nonce} caps a wallet at ` +
        `${whole(BigInt(sale.maxContribution.toString()), USDC_DECIMALS)} USDC, below this draw`,
    );
  }
  if (stateName(sale.state) === "softCapMissed") {
    throw new Error(
      `sale nonce ${nonce} resolved SoftCapMissed — claim is permanently forbidden. ` +
        "Add a draw on a new nonce; do not try to salvage this one.",
    );
  }

  // ---- Step 2: fund the contribution -------------------------------------
  console.log("--- Step 2: test-USDC for the contribution ---");
  const contributionInfo = await connection.getAccountInfo(contributionPda);
  const alreadyContributed = contributionInfo
    ? BigInt((await accounts.contribution.fetch(contributionPda)).amountUsdc.toString())
    : 0n;
  const stillToContribute = usdcUnits(usdcFor(openAmount)) - alreadyContributed;

  if (stillToContribute <= 0n) {
    console.log(`already contributed ${whole(alreadyContributed, USDC_DECIMALS)} USDC — skipping`);
  } else {
    const usdcHeld = await tokenBalance(connection, ctx.adminUsdcAta);
    console.log(
      `admin holds ${whole(usdcHeld, USDC_DECIMALS)} USDC, needs ${whole(stillToContribute, USDC_DECIMALS)}`,
    );
    if (usdcHeld < stillToContribute) {
      const shortfall = stillToContribute - usdcHeld;
      const sig = await mintTo(
        connection, admin, ctx.usdcMint, ctx.adminUsdcAta, admin, shortfall,
        [], { commitment: "confirmed" }, TOKEN_2022_PROGRAM_ID,
      );
      signatures.mintUsdc = sig;
      console.log(`minted ${whole(shortfall, USDC_DECIMALS)} test-USDC: ${sig}`);
    } else {
      console.log("no minting needed");
    }

    // ---- Step 3: contribute ---------------------------------------------
    console.log("--- Step 3: contribute ---");
    const sig = await program.methods
      .contributeUsdc(new BN(nonce), bn(stillToContribute))
      .accountsPartial({
        buyer: admin.publicKey,
        banRecord: ctx.banRecord,
        saleConfig: saleConfigPda,
        buyerUsdc: ctx.adminUsdcAta,
        usdcVault,
        usdcMint: ctx.usdcMint,
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
  if (entitlement < openUnits(openAmount)) {
    throw new Error(
      `entitlement ${whole(entitlement, OPEN_DECIMALS)} OPEN is below this draw — ` +
        "the contribution did not fully land",
    );
  }

  // ---- Step 4: finalize --------------------------------------------------
  console.log("--- Step 4: finalize ---");
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
      .finalizeSale(new BN(nonce))
      .accountsPartial({
        admin: admin.publicKey,
        saleConfig: saleConfigPda,
        usdcVault,
        treasury: ctx.adminUsdcAta,
        usdcMint: ctx.usdcMint,
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
  console.log("--- Step 5: claim ---");
  if (contribution.claimed) {
    console.log("entitlement already claimed — skipping");
  } else {
    if (!(await connection.getAccountInfo(ctx.adminOpenAta))) {
      const tx = new Transaction().add(
        createAssociatedTokenAccountIdempotentInstruction(
          admin.publicKey, ctx.adminOpenAta, admin.publicKey, ctx.openMint, TOKEN_2022_PROGRAM_ID,
        ),
      );
      const sig = await sendAndConfirmTransaction(connection, tx, [admin], { commitment: "confirmed" });
      signatures.createAdminOpenAta = sig;
      console.log(`created admin OPEN ATA ${ctx.adminOpenAta.toBase58()}: ${sig}`);
    }
    const sig = await program.methods
      .claim(new BN(nonce))
      .accountsPartial({
        buyer: admin.publicKey,
        saleConfig: saleConfigPda,
        openMint: ctx.openMint,
        presaleVaultAuthority: ctx.presaleVaultAuthority,
        presaleVault: ctx.presaleVault,
        contribution: contributionPda,
        buyerOpen: ctx.adminOpenAta,
        tokenProgram: TOKEN_2022_PROGRAM_ID,
      })
      .rpc({ commitment: "confirmed" });
    signatures.claim = sig;
    console.log(`claim: ${sig}`);
  }

  return signatures;
}

async function main() {
  for (const draw of DRAWS) {
    if (PROTECTED_SALE_NONCES.includes(draw.nonce)) {
      throw new Error(
        `refusing to run against sale nonce ${draw.nonce}: it is a protected sale ` +
          "(nonce 1 is the live tester-facing presale and must stay Active until it " +
          "closes). This script finalizes the sale it uses, which cannot be undone. " +
          "Pick an unused nonce.",
      );
    }
    if (draw.openAmount > PRESALE_BUCKET_OPEN) {
      throw new Error(
        `draw ${draw.openAmount} exceeds the ${PRESALE_BUCKET_OPEN} OPEN in the bucket`,
      );
    }
  }
  if (TOTAL_TARGET_OPEN > PRESALE_BUCKET_OPEN) {
    throw new Error(
      `draws total ${TOTAL_TARGET_OPEN} OPEN, more than the ${PRESALE_BUCKET_OPEN} in the bucket`,
    );
  }
  if (new Set(DRAWS.map((d) => d.nonce)).size !== DRAWS.length) {
    throw new Error("two draws share a sale nonce — each needs its own");
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
  console.log(
    `Target: ${TOTAL_TARGET_OPEN.toLocaleString("en-US")} OPEN across ${DRAWS.length} draw(s) ` +
      `at nonce(s) ${DRAWS.map((d) => d.nonce).join(", ")}\n`,
  );

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

  // ---- Step 0: verify every assumption these cycles rest on --------------
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
    connection, presaleVault, "confirmed", TOKEN_2022_PROGRAM_ID,
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
  const vaultAtStart = vaultAccount.amount;
  console.log(
    `Presale vault ${presaleVault.toBase58()}: ${whole(vaultAtStart, OPEN_DECIMALS)} OPEN ` +
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
  const faucetOpenAtStart = await tokenBalance(connection, faucetOpenAta);
  console.log(
    `Faucet OPEN ATA ${faucetOpenAta.toBase58()}: ${whole(faucetOpenAtStart, OPEN_DECIMALS)} OPEN`,
  );

  // The vault must still be able to cover whatever hasn't been drawn yet.
  const outstanding = openUnits(TOTAL_TARGET_OPEN) - faucetOpenAtStart;
  if (outstanding > 0n && vaultAtStart < outstanding) {
    throw new Error(
      `presale vault holds ${whole(vaultAtStart, OPEN_DECIMALS)} OPEN but ` +
        `${whole(outstanding, OPEN_DECIMALS)} is still owed to reach the target`,
    );
  }

  const wallet = new anchor.Wallet(admin);
  const provider = new anchor.AnchorProvider(connection, wallet, { commitment: "confirmed" });
  const program = new anchor.Program(await loadIdl(provider), provider);

  // Proof-of-non-existence ban gate (OFS-7100 §12): the buyer is banned iff
  // this governance PDA is occupied. Passing it is mandatory even when — as
  // here — it is empty.
  const [banRecord] = PublicKey.findProgramAddressSync(
    [Buffer.from("ban"), admin.publicKey.toBytes()], GOVERNANCE_PROGRAM_ID,
  );

  const ctx: Context = {
    connection, admin, program,
    accounts: program.account as unknown as PresaleAccounts,
    openMint, usdcMint, presaleVault, presaleVaultAuthority,
    adminUsdcAta, adminOpenAta, banRecord,
  };

  const perDrawSignatures: Record<number, Record<string, string>> = {};
  for (const draw of DRAWS) {
    perDrawSignatures[draw.nonce] = await runDrawCycle(ctx, draw);
  }

  const adminOpen = await tokenBalance(connection, adminOpenAta);
  console.log(`\nadmin now holds ${whole(adminOpen, OPEN_DECIMALS)} OPEN`);

  // ---- Step 6: hand it to the faucet ------------------------------------
  console.log("\n=== Step 6: transfer to the faucet authority ===");
  const transferSignatures: string[] = [];
  const faucetShortfall = openUnits(TOTAL_TARGET_OPEN) - faucetOpenAtStart;
  if (faucetShortfall <= 0n) {
    console.log(
      `faucet already holds ${whole(faucetOpenAtStart, OPEN_DECIMALS)} OPEN — nothing to send`,
    );
  } else {
    const toSend = adminOpen < faucetShortfall ? adminOpen : faucetShortfall;
    if (toSend <= 0n) {
      throw new Error("admin holds no OPEN to forward — the claims did not deliver");
    }
    const tx = new Transaction().add(
      // Idempotent: the faucet's OPEN ATA may already exist from an earlier run
      // or from manual setup on the VPS. Its address is deterministic either
      // way, so creating it here is equivalent to creating it there.
      createAssociatedTokenAccountIdempotentInstruction(
        admin.publicKey, faucetOpenAta, FAUCET_AUTHORITY, openMint, TOKEN_2022_PROGRAM_ID,
      ),
      createTransferCheckedInstruction(
        adminOpenAta, openMint, faucetOpenAta, admin.publicKey,
        toSend, OPEN_DECIMALS, [], TOKEN_2022_PROGRAM_ID,
      ),
    );
    const sig = await sendAndConfirmTransaction(connection, tx, [admin], { commitment: "confirmed" });
    transferSignatures.push(sig);
    console.log(`transferred ${whole(toSend, OPEN_DECIMALS)} OPEN to the faucet: ${sig}`);
  }

  // ---- Step 7: verify by reading the chain, not by trusting the sends ----
  console.log("\n=== Step 7: verification (balances read back from chain) ===");
  const faucetOpenAfter = await tokenBalance(connection, faucetOpenAta);
  const vaultAfter = await tokenBalance(connection, presaleVault);
  console.log(`presale vault: ${whole(vaultAtStart, OPEN_DECIMALS)} → ${whole(vaultAfter, OPEN_DECIMALS)} OPEN`);
  console.log(`faucet OPEN:   ${whole(faucetOpenAtStart, OPEN_DECIMALS)} → ${whole(faucetOpenAfter, OPEN_DECIMALS)} OPEN`);

  if (faucetOpenAfter < openUnits(TOTAL_TARGET_OPEN)) {
    throw new Error(
      `faucet holds ${whole(faucetOpenAfter, OPEN_DECIMALS)} OPEN, expected at least ` +
        `${TOTAL_TARGET_OPEN.toLocaleString("en-US")} — the transfer did not deliver`,
    );
  }
  console.log(`drawn from the vault this run: ${whole(vaultAtStart - vaultAfter, OPEN_DECIMALS)} OPEN`);

  // ---- Step 8: record why the bucket is lighter -------------------------
  // This record IS the audit trail, so a re-run must never destroy it. Being
  // idempotent, a second run performs no transactions and observes a vault that
  // is already drawn down — writing that naively would report before == after
  // and blank out the signatures that prove the draws ever happened. So the
  // first run's observations and every signature seen are merged forward rather
  // than overwritten.
  const prior = (allAddresses.faucet_open_draw ?? {}) as Record<string, unknown>;
  const priorDraws = (prior.draws ?? []) as { saleNonce: number; signatures?: Record<string, string> }[];
  const priorByNonce = new Map(priorDraws.map((d) => [d.saleNonce, d]));

  allAddresses.faucet_open_draw = {
    reason:
      "Devnet faucet tooling, NOT an accounting anomaly. OPEN's mint authority is " +
      "permanently unset, so the devnet faucet cannot mint OPEN the way it mints mock " +
      "USDC/USDT — the only movable OPEN on devnet is the Community Presale bucket, and " +
      "the only way out of it is claim() on a Finalized sale. Each draw below is the " +
      "proceeds of one deliberate, fully-executed presale cycle on its own nonce, run " +
      "purely to stock the faucet. Nothing was sold and nothing is owed to anyone. " +
      "Reproduce or audit with scripts/extract-open-for-faucet.ts.",
    sizing:
      `${TOTAL_TARGET_OPEN} OPEN is ${Math.floor(TOTAL_TARGET_OPEN / 12_000)} grants at the ` +
      "faucet's 12,000 OPEN per-request drip. 12,000 is a deliberate boundary against the live " +
      "StakingConfig (2wrGGjcUFSn1ZiYzo2o64r7ZC88QNhvvgUYktNs2ifT9): it buys EXACTLY arbitrator " +
      "+ merchant + node operator (10,000 + 1,000 + 1,000) with zero slack. It does NOT cover " +
      "arbitrator + notification provider (15,000) or all seven roles (20,000) — those need a " +
      "second grant the next day, since the cap is per 24h. Because 12,000 is exact for that " +
      "three-role bundle, nothing may be deducted from an OPEN grant for fees or rent; those " +
      "come out of the faucet's separate SOL grant.",
    moreAvailable:
      "This is not an irreversible commitment. The presale_vault PDA is a singleton with no " +
      "nonce, shared by every sale, and initialize_sale only verifies its mint and owner " +
      "rather than funding it — so any future sale we open as admin reaches the same vault, " +
      "and topping the faucet up later is one more cycle on a fresh nonce. Add it to DRAWS " +
      "in the script; do not try to re-open a finalized sale, which is impossible.",
    openDrawnTotal: TOTAL_TARGET_OPEN,
    presaleVault: presaleVault.toBase58(),
    presaleVaultOpenBefore:
      prior.presaleVaultOpenBefore ?? Number(vaultAtStart) / 10 ** OPEN_DECIMALS,
    presaleVaultOpenAfter: Number(vaultAfter) / 10 ** OPEN_DECIMALS,
    faucetAuthority: FAUCET_AUTHORITY.toBase58(),
    faucetOpenAta: faucetOpenAta.toBase58(),
    faucetOpenBalance: Number(faucetOpenAfter) / 10 ** OPEN_DECIMALS,
    draws: DRAWS.map((d) => {
      const previous = priorByNonce.get(d.nonce);
      const [saleConfigPda] = PublicKey.findProgramAddressSync(
        [Buffer.from("sale_config"), leU64(d.nonce)], PRESALE_PROGRAM_ID,
      );
      return {
        saleNonce: d.nonce,
        saleConfig: saleConfigPda.toBase58(),
        openDrawn: d.openAmount,
        usdcContributed: d.openAmount,
        signatures: { ...(previous?.signatures ?? {}), ...perDrawSignatures[d.nonce] },
      };
    }),
    transferToFaucetSignatures: [
      ...((prior.transferToFaucetSignatures ?? []) as string[]),
      ...transferSignatures,
    ],
    executedAt: prior.executedAt ?? new Date().toISOString(),
    lastVerifiedAt: new Date().toISOString(),
  };
  fs.writeFileSync(addrPath, JSON.stringify(allAddresses, null, 2) + "\n");
  console.log("\nRecorded faucet_open_draw in devnet-addresses.json");

  console.log("\n=== Signatures this run ===");
  let any = false;
  for (const draw of DRAWS) {
    for (const [step, sig] of Object.entries(perDrawSignatures[draw.nonce])) {
      console.log(`nonce ${draw.nonce} ${step}: ${sig}`);
      any = true;
    }
  }
  for (const sig of transferSignatures) {
    console.log(`transferToFaucet: ${sig}`);
    any = true;
  }
  if (!any) console.log("(none — everything was already done on a previous run)");
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
