/**
 * One-time devnet setup for `openfiat-governance`: calls
 * `initialize_governance_config` against the newly (re)deployed governance
 * program, seeding the singleton `GovernanceConfig` and `deposit_vault`
 * PDAs, and (atomically, in the same instruction) AllenHark's first-year
 * `EmergencyAuthority` (OFS-4100 §5.1).
 *
 * Values are OFS-4100 §5's signed-off governance figures:
 * `Whitepaper/Specifications/OFS-4100 - OpenFiat Tokenomics Specification
 * (OTS).md`. See
 * `programs/governance/src/instructions/initialize_governance_config.rs`
 * for the exact param/account layout this script drives.
 *
 * `total_open_supply` is 100,000,000,000 OPEN at the re-baselined
 * (2026-08-09) 6-decimal precision — 1e17 base units, which exceeds
 * `Number.MAX_SAFE_INTEGER` (~9.007e15), so it is built with `BN`, never a
 * JS number literal.
 *
 * `forfeit_destination` is set to the admin (EA8Ty)'s own OPEN Token-2022
 * ATA. This is a **devnet placeholder** — OFS-4100 §5 specifies forfeited
 * proposal deposits go to the Ecosystem Treasury, but that bucket's owner
 * is a separate signer this script doesn't assume access to; the
 * destination only needs to be *a* valid OPEN token account for the
 * program to accept it.
 *
 * `initialize_governance_config` has no voting-period parameter — voting
 * period is per-proposal (`create_proposal`'s `voting_period_secs`,
 * bounded below by the compile-time `MIN_VOTING_PERIOD_SECS`), not part of
 * the singleton config, so there is nothing to set for it here.
 *
 * Usage: npx ts-node scripts/init-devnet-governance.ts
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
import {
  TOKEN_2022_PROGRAM_ID,
  getOrCreateAssociatedTokenAccount,
} from "@solana/spl-token";
import * as anchor from "@anchor-lang/core";
import { BN } from "@anchor-lang/core";

const GOVERNANCE_PROGRAM_ID = new PublicKey(
  "2k71DBDoxM4SUFYGbyMXFiTSUynPuY2CqFUsx3FuarXF",
);

// OFS-4100 §1, re-baselined 2026-08-09: OPEN is 6 decimals, matching USDC.
const OPEN_DECIMALS_MULTIPLIER = new BN(1_000_000);
const openUnits = (whole: number) => new BN(whole).mul(OPEN_DECIMALS_MULTIPLIER);

// OFS-4100 §1/§5 (re-baselined 2026-08-09): 100B OPEN total supply.
const TOTAL_OPEN_SUPPLY = new BN("100000000000").mul(OPEN_DECIMALS_MULTIPLIER);
// OFS-4100 §5: "Quorum: 10% of circulating staked-for-governance supply".
const QUORUM_BPS = 1_000;
// OFS-4100 §5: "Approval threshold — Informational, Standards, Parameter:
// Simple majority (>50%)".
const THRESHOLD_SIMPLE_BPS = 5_000;
// OFS-4100 §5: "Approval threshold — Treasury: 60% supermajority".
const THRESHOLD_TREASURY_BPS = 6_000;
// OFS-4100 §5: "Approval threshold — Protocol-Upgrade, Constitutional: 66%
// supermajority + 20% quorum (higher than the standard 10%)".
const THRESHOLD_UPGRADE_BPS = 6_600;
const QUORUM_UPGRADE_BPS = 2_000;
// OFS-4100 §5: "Proposal stake deposit: 5,000 OPEN".
const DEPOSIT_AMOUNT = openUnits(5_000);
// OFS-4100 §5: "Vote-lock duration: 7 days (matches the voting period)".
// Bounded above by MAX_VOTE_LOCK_SECS (30 days) — enforced on-chain.
const VOTE_LOCK_SECS = new BN(7 * 24 * 60 * 60);

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

  console.log("\nCreating/looking up admin's OPEN ATA (devnet forfeit_destination placeholder)...");
  const forfeitDestination = await getOrCreateAssociatedTokenAccount(
    connection,
    admin,
    openMint,
    admin.publicKey,
    false,
    undefined,
    undefined,
    TOKEN_2022_PROGRAM_ID,
  );
  console.log(`forfeit_destination (admin's OPEN ATA): ${forfeitDestination.address.toBase58()}`);

  const wallet = new anchor.Wallet(admin);
  const provider = new anchor.AnchorProvider(connection, wallet, {
    commitment: "confirmed",
  });
  const idl = JSON.parse(
    fs.readFileSync(
      path.join(__dirname, "..", "target", "idl", "governance.json"),
      "utf-8",
    ),
  );
  const program = new anchor.Program(idl, provider);

  const [governanceConfig] = PublicKey.findProgramAddressSync(
    [Buffer.from("governance_config")],
    GOVERNANCE_PROGRAM_ID,
  );
  const [depositVault] = PublicKey.findProgramAddressSync(
    [Buffer.from("deposit_vault")],
    GOVERNANCE_PROGRAM_ID,
  );
  const [emergencyAuthority] = PublicKey.findProgramAddressSync(
    [Buffer.from("emergency_authority")],
    GOVERNANCE_PROGRAM_ID,
  );

  const existing = await connection.getAccountInfo(governanceConfig);
  if (existing) {
    console.log(`\ngovernance_config already initialized at ${governanceConfig.toBase58()} — skipping initialize_governance_config.`);
  } else {
    console.log("\nCalling initialize_governance_config...");
    await program.methods
      .initializeGovernanceConfig({
        totalOpenSupply: TOTAL_OPEN_SUPPLY,
        quorumBps: QUORUM_BPS,
        thresholdSimpleBps: THRESHOLD_SIMPLE_BPS,
        thresholdTreasuryBps: THRESHOLD_TREASURY_BPS,
        thresholdUpgradeBps: THRESHOLD_UPGRADE_BPS,
        quorumUpgradeBps: QUORUM_UPGRADE_BPS,
        depositAmount: DEPOSIT_AMOUNT,
        forfeitDestination: forfeitDestination.address,
        voteLockSecs: VOTE_LOCK_SECS,
      })
      .accountsPartial({
        admin: admin.publicKey,
        mint: openMint,
        governanceConfig,
        depositVault,
        emergencyAuthority,
        tokenProgram: TOKEN_2022_PROGRAM_ID,
        systemProgram: anchor.web3.SystemProgram.programId,
        rent: SYSVAR_RENT_PUBKEY,
      })
      .rpc({ commitment: "confirmed" });
    console.log("Governance config (and first-year EmergencyAuthority) initialized.");
  }

  console.log("\nVerifying on-chain GovernanceConfig and EmergencyAuthority...");
  const onChainConfig: any = await (program.account as any).governanceConfig.fetch(
    governanceConfig,
  );
  const onChainEmergency: any = await (program.account as any).emergencyAuthority.fetch(
    emergencyAuthority,
  );

  const assertEq = (label: string, actual: unknown, expected: unknown) => {
    if (String(actual) !== String(expected)) {
      throw new Error(`Assertion failed: ${label} — on-chain ${actual}, expected ${expected}`);
    }
  };
  assertEq("admin", onChainConfig.admin.toBase58(), admin.publicKey.toBase58());
  assertEq("mint", onChainConfig.mint.toBase58(), openMint.toBase58());
  assertEq("totalOpenSupply", onChainConfig.totalOpenSupply.toString(), TOTAL_OPEN_SUPPLY.toString());
  assertEq("quorumBps", onChainConfig.quorumBps, QUORUM_BPS);
  assertEq("thresholdSimpleBps", onChainConfig.thresholdSimpleBps, THRESHOLD_SIMPLE_BPS);
  assertEq("thresholdTreasuryBps", onChainConfig.thresholdTreasuryBps, THRESHOLD_TREASURY_BPS);
  assertEq("thresholdUpgradeBps", onChainConfig.thresholdUpgradeBps, THRESHOLD_UPGRADE_BPS);
  assertEq("quorumUpgradeBps", onChainConfig.quorumUpgradeBps, QUORUM_UPGRADE_BPS);
  assertEq("depositAmount", onChainConfig.depositAmount.toString(), DEPOSIT_AMOUNT.toString());
  assertEq(
    "forfeitDestination",
    onChainConfig.forfeitDestination.toBase58(),
    forfeitDestination.address.toBase58(),
  );
  assertEq("voteLockSecs", onChainConfig.voteLockSecs.toString(), VOTE_LOCK_SECS.toString());
  assertEq(
    "emergencyAuthority.primaryHolder",
    onChainEmergency.primaryHolder.toBase58(),
    "ALLENLMtV1zEAHT3xpVryqcbdPCB8c9JhM1Jdbe5XHg5",
  );
  assertEq(
    "emergencyAuthority.secondaryHolder",
    onChainEmergency.secondaryHolder.toBase58(),
    "A11ENCKCBxZxEbXQmqs6mTmJkP8gjcA7xqfLD5BxfRpp",
  );
  console.log("On-chain GovernanceConfig and EmergencyAuthority match expected values.");

  const out = {
    programId: GOVERNANCE_PROGRAM_ID.toBase58(),
    openMint: openMint.toBase58(),
    governanceConfig: governanceConfig.toBase58(),
    depositVault: depositVault.toBase58(),
    emergencyAuthority: emergencyAuthority.toBase58(),
    forfeitDestination: forfeitDestination.address.toBase58(),
    totalOpenSupply: TOTAL_OPEN_SUPPLY.toString(),
    quorumBps: QUORUM_BPS,
    thresholdSimpleBps: THRESHOLD_SIMPLE_BPS,
    thresholdTreasuryBps: THRESHOLD_TREASURY_BPS,
    thresholdUpgradeBps: THRESHOLD_UPGRADE_BPS,
    quorumUpgradeBps: QUORUM_UPGRADE_BPS,
    depositAmount: DEPOSIT_AMOUNT.toString(),
    voteLockSecs: VOTE_LOCK_SECS.toString(),
    emergencyAuthorityPrimaryHolder: "ALLENLMtV1zEAHT3xpVryqcbdPCB8c9JhM1Jdbe5XHg5",
    emergencyAuthoritySecondaryHolder: "A11ENCKCBxZxEbXQmqs6mTmJkP8gjcA7xqfLD5BxfRpp",
  };
  allAddresses.devnet_governance = out;
  fs.writeFileSync(addrPath, JSON.stringify(allAddresses, null, 2) + "\n");
  console.log("\nWrote devnet_governance entry to devnet-addresses.json:");
  console.log(JSON.stringify(out, null, 2));
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
