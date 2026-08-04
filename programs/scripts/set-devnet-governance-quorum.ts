/**
 * Lowers the devnet governance quorum so the full proposal lifecycle —
 * create, vote, tally, settle the deposit — can actually be exercised.
 *
 * # The problem
 *
 * Quorum is a fraction of the *total OPEN supply*, not of what is staked.
 * At 1,000 bps that is 100,000,000 OPEN against roughly 102,000 staked
 * across the whole devnet cluster, so `tally_and_finalize` could only ever
 * resolve `Rejected` on a quorum miss. Every proposal ever created on devnet
 * was unpassable by arithmetic, which means the accept branch, the
 * threshold comparison, and the deposit *refund* path had never run against
 * real state and could not be made to.
 *
 * Mainnet is not affected and this script cannot touch it: it refuses any
 * non-devnet endpoint, and the real quorum is a governance parameter that a
 * mainnet deployment sets for itself.
 *
 * # The values, and why not simply zero
 *
 * Zero quorum would be the easy answer and the wrong one. `quorum_met`
 * becomes unconditionally true, and the quorum *miss* path — which is what
 * decides whether a proposer's 5,000 OPEN deposit is refunded or forfeited —
 * stops being reachable at all. Lowering a threshold should keep both
 * branches testable, not delete one.
 *
 * 1 bps of 1,000,000,000 OPEN is 100,000 OPEN, chosen so that both hold:
 *
 *  - a single fully-staked node operator (100,000 OPEN) meets it exactly,
 *    so the accept path is provable today;
 *  - a lone tester holding one 12,000 OPEN faucet grant does not, so the
 *    quorum-miss path stays provable too.
 *
 * `quorum_upgrade_bps` keeps its 2x relationship to the standard quorum
 * (2 bps, 200,000 OPEN). That is deliberately *not* reachable by one voter
 * today: protocol-upgrade proposals are meant to need broader participation
 * than ordinary ones, and flattening that to make a demo easier would be
 * changing the mechanism rather than rescaling it. It becomes reachable as
 * testers stake their grants.
 *
 * Every other field is echoed back unchanged. `vote_lock_secs` especially:
 * writing it is the OFS-4100 §5.1 delay power, and the program refuses a
 * *change* to it after the sunset lapses, so a script that got sloppy about
 * round-tripping it would start failing on a date rather than on a mistake.
 *
 * Note this affects **new proposals only**. `quorum_snapshot` is captured
 * into each `Proposal` at creation, precisely so that moving the parameter
 * cannot retroactively change the bar a live vote was cast under.
 *
 * Usage:
 *   npx ts-node scripts/set-devnet-governance-quorum.ts [--commit]
 */
import * as fs from "fs";
import * as path from "path";
import { Connection, Keypair, PublicKey } from "@solana/web3.js";
import { TOKEN_2022_PROGRAM_ID } from "@solana/spl-token";
import * as anchor from "@anchor-lang/core";
import { BN } from "@anchor-lang/core";

const GOVERNANCE_PROGRAM_ID = new PublicKey(
  "AVJfKUjHsizkGGUy8sdz4Xma2hVgmgvgg8GmUMs8E4eE",
);

/** 1 bps of the 1,000,000,000 OPEN supply = 100,000 OPEN. */
const NEW_QUORUM_BPS = 1;
/** Keeps the 2x relationship the deployed config already expressed. */
const NEW_QUORUM_UPGRADE_BPS = 2;

const OPEN_DECIMALS = 9;
const BPS_DENOMINATOR = 10_000;

interface GovernanceConfigAccount {
  admin: PublicKey;
  mint: PublicKey;
  totalOpenSupply: BN;
  quorumBps: number;
  thresholdSimpleBps: number;
  thresholdTreasuryBps: number;
  thresholdUpgradeBps: number;
  quorumUpgradeBps: number;
  depositAmount: BN;
  forfeitDestination: PublicKey;
  voteLockSecs: BN;
}

const openFromBps = (supply: bigint, bps: number) =>
  Number((supply * BigInt(bps)) / BigInt(BPS_DENOMINATOR)) / 10 ** OPEN_DECIMALS;

async function main() {
  const commit = process.argv.includes("--commit");
  const rpc = process.env.ANCHOR_PROVIDER_URL ?? "https://api.devnet.solana.com";
  if (!rpc.includes("devnet")) {
    throw new Error(
      `refusing to run against ${rpc}: this script is devnet-only, and quorum on any ` +
        "other cluster is a governance decision rather than a tooling one",
    );
  }

  const admin = Keypair.fromSecretKey(
    Uint8Array.from(
      JSON.parse(
        fs.readFileSync(
          process.env.SOLANA_KEYPAIR ||
            path.join(process.env.HOME || "~", ".config/solana/id.json"),
          "utf-8",
        ),
      ),
    ),
  );
  const connection = new Connection(rpc, "confirmed");
  const provider = new anchor.AnchorProvider(connection, new anchor.Wallet(admin), {
    commitment: "confirmed",
  });
  const idlPath = path.join(__dirname, "..", "target", "idl", "governance.json");
  if (!fs.existsSync(idlPath)) throw new Error("no target/idl/governance.json — run `anchor build`");
  const program = new anchor.Program(
    JSON.parse(fs.readFileSync(idlPath, "utf-8")) as anchor.Idl,
    provider,
  );
  const accounts = program.account as unknown as {
    governanceConfig: { fetch(a: PublicKey): Promise<GovernanceConfigAccount> };
  };

  const [configPda] = PublicKey.findProgramAddressSync(
    [Buffer.from("governance_config")],
    GOVERNANCE_PROGRAM_ID,
  );
  const [emergencyAuthority] = PublicKey.findProgramAddressSync(
    [Buffer.from("emergency_authority")],
    GOVERNANCE_PROGRAM_ID,
  );

  const config = await accounts.governanceConfig.fetch(configPda);
  const supply = BigInt(config.totalOpenSupply.toString());

  console.log(`rpc    : ${rpc}`);
  console.log(`admin  : ${admin.publicKey.toBase58()}`);
  console.log(`config : ${configPda.toBase58()}`);
  if (config.admin.toBase58() !== admin.publicKey.toBase58()) {
    throw new Error(
      `config admin is ${config.admin.toBase58()}, not this keypair — it cannot update the config`,
    );
  }

  console.log("\ncurrent:");
  console.log(
    `  quorum_bps         ${config.quorumBps}  (${openFromBps(supply, config.quorumBps).toLocaleString("en-US")} OPEN)`,
  );
  console.log(
    `  quorum_upgrade_bps ${config.quorumUpgradeBps}  (${openFromBps(supply, config.quorumUpgradeBps).toLocaleString("en-US")} OPEN)`,
  );
  console.log("\nproposed:");
  console.log(
    `  quorum_bps         ${NEW_QUORUM_BPS}  (${openFromBps(supply, NEW_QUORUM_BPS).toLocaleString("en-US")} OPEN)`,
  );
  console.log(
    `  quorum_upgrade_bps ${NEW_QUORUM_UPGRADE_BPS}  (${openFromBps(supply, NEW_QUORUM_UPGRADE_BPS).toLocaleString("en-US")} OPEN)`,
  );
  console.log("\nunchanged: thresholds, deposit_amount, total_open_supply, vote_lock_secs");

  if (config.quorumBps === NEW_QUORUM_BPS && config.quorumUpgradeBps === NEW_QUORUM_UPGRADE_BPS) {
    console.log("\nalready set — nothing to do.");
    return;
  }
  if (!commit) {
    console.log("\nDRY RUN — re-run with --commit to write it.");
    return;
  }

  const sig = await program.methods
    .updateGovernanceConfig({
      // Echoed back verbatim. Only the two quorum fields are this script's
      // business; anything else differing from what was read would be a
      // silent parameter change riding along with an unrelated one.
      totalOpenSupply: config.totalOpenSupply,
      quorumBps: NEW_QUORUM_BPS,
      thresholdSimpleBps: config.thresholdSimpleBps,
      thresholdTreasuryBps: config.thresholdTreasuryBps,
      thresholdUpgradeBps: config.thresholdUpgradeBps,
      quorumUpgradeBps: NEW_QUORUM_UPGRADE_BPS,
      depositAmount: config.depositAmount,
      voteLockSecs: config.voteLockSecs,
    })
    .accountsPartial({
      admin: admin.publicKey,
      governanceConfig: configPda,
      mint: config.mint,
      forfeitDestination: config.forfeitDestination,
      emergencyAuthority,
      tokenProgram: TOKEN_2022_PROGRAM_ID,
    })
    .rpc({ commitment: "confirmed" });
  console.log(`\nupdate_governance_config: ${sig}`);

  // Read it back. A confirmed transaction proves it landed, not that the
  // fields hold what was intended.
  const after = await accounts.governanceConfig.fetch(configPda);
  const failures: string[] = [];
  if (after.quorumBps !== NEW_QUORUM_BPS) failures.push("quorum_bps did not change");
  if (after.quorumUpgradeBps !== NEW_QUORUM_UPGRADE_BPS)
    failures.push("quorum_upgrade_bps did not change");
  if (after.thresholdSimpleBps !== config.thresholdSimpleBps)
    failures.push("threshold_simple_bps changed and should not have");
  if (after.depositAmount.toString() !== config.depositAmount.toString())
    failures.push("deposit_amount changed and should not have");
  if (after.voteLockSecs.toString() !== config.voteLockSecs.toString())
    failures.push("vote_lock_secs changed and should not have");
  if (after.totalOpenSupply.toString() !== config.totalOpenSupply.toString())
    failures.push("total_open_supply changed and should not have");
  if (failures.length > 0) {
    for (const f of failures) console.error(`  FAIL: ${f}`);
    throw new Error(`${failures.length} check(s) failed`);
  }

  console.log(
    `\nverified: quorum is now ${openFromBps(supply, after.quorumBps).toLocaleString("en-US")} OPEN ` +
      `(${openFromBps(supply, after.quorumUpgradeBps).toLocaleString("en-US")} for upgrades), ` +
      "and nothing else moved.",
  );
  console.log(
    "applies to proposals created from now on — existing ones keep the quorum_snapshot " +
      "they were created under.",
  );
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
