/**
 * One-time devnet setup for `openfiat-staking`: calls
 * `initialize_staking_config` against the newly (re)deployed staking
 * program, seeding the singleton `StakingConfig` plus its `stake_vault`
 * and `rewards_vault` PDAs.
 *
 * Values mirror `staking::constants::RECOMMENDED_MIN_STAKE_BY_ROLE`,
 * `RECOMMENDED_UNBONDING_PERIOD_SECS_BY_ROLE` and `RECOMMENDED_SLASH_BPS`
 * (OFS-4100 §4, re-baselined 2026-08-09 — static USD-equivalent minimums
 * pinned at the $0.01 presale price, 6-decimal OPEN). See
 * `programs/staking/src/instructions/initialize_staking_config.rs` for the
 * exact param/account layout this script drives.
 *
 * `slash_destination` is set to the admin (EA8Ty)'s own OPEN Token-2022
 * ATA. This is a **devnet placeholder** — mainnet wants a dedicated
 * emergency-reserve ATA distinct from any single signer's wallet, but that
 * account doesn't need to exist yet for devnet ceremony testing, and the
 * destination only needs to be *a* valid OPEN token account for the
 * program to accept it.
 *
 * Usage: npx ts-node scripts/init-devnet-staking.ts
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

const STAKING_PROGRAM_ID = new PublicKey(
  "3MF1nAPiECRAGs36RpQTkLb8CvMZuhcgXv1hGZoXEiid",
);

// OFS-4100 §1, re-baselined 2026-08-09: OPEN is 6 decimals, matching USDC.
const OPEN_DECIMALS_MULTIPLIER = new BN(1_000_000);
const openUnits = (whole: number) => new BN(whole).mul(OPEN_DECIMALS_MULTIPLIER);

// Indexed by Role::index — Merchant, Arbitrator, NodeOperator,
// NotificationProvider, OracleProvider, RiskIntelligenceProvider,
// SnapshotProvider. Mirrors RECOMMENDED_MIN_STAKE_BY_ROLE.
const ROLE_NAMES = [
  "Merchant",
  "Arbitrator",
  "NodeOperator",
  "NotificationProvider",
  "OracleProvider",
  "RiskIntelligenceProvider",
  "SnapshotProvider",
];
const MIN_STAKE_BY_ROLE = [500, 100_000, 500_000, 100_000, 100_000, 100_000, 100_000].map(
  openUnits,
);
// Mirrors RECOMMENDED_UNBONDING_PERIOD_SECS_BY_ROLE, in seconds.
const UNBONDING_PERIOD_SECS_BY_ROLE = [
  86400, 259200, 604800, 604800, 604800, 604800, 604800,
].map((secs) => new BN(secs));
// Mirrors RECOMMENDED_SLASH_BPS.
const SLASH_BPS = 500;

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

  console.log("\nCreating/looking up admin's OPEN ATA (devnet slash_destination placeholder)...");
  const slashDestination = await getOrCreateAssociatedTokenAccount(
    connection,
    admin,
    openMint,
    admin.publicKey,
    false,
    undefined,
    undefined,
    TOKEN_2022_PROGRAM_ID,
  );
  console.log(`slash_destination (admin's OPEN ATA): ${slashDestination.address.toBase58()}`);

  const wallet = new anchor.Wallet(admin);
  const provider = new anchor.AnchorProvider(connection, wallet, {
    commitment: "confirmed",
  });
  const idl = JSON.parse(
    fs.readFileSync(
      path.join(__dirname, "..", "target", "idl", "staking.json"),
      "utf-8",
    ),
  );
  const program = new anchor.Program(idl, provider);

  const [stakingConfig] = PublicKey.findProgramAddressSync(
    [Buffer.from("staking_config")],
    STAKING_PROGRAM_ID,
  );
  const [stakeVault] = PublicKey.findProgramAddressSync(
    [Buffer.from("stake_vault")],
    STAKING_PROGRAM_ID,
  );
  const [rewardsVault] = PublicKey.findProgramAddressSync(
    [Buffer.from("rewards_vault")],
    STAKING_PROGRAM_ID,
  );

  const existing = await connection.getAccountInfo(stakingConfig);
  if (existing) {
    console.log(`\nstaking_config already initialized at ${stakingConfig.toBase58()} — skipping initialize_staking_config.`);
  } else {
    console.log("\nCalling initialize_staking_config...");
    await program.methods
      .initializeStakingConfig({
        minStakeByRole: MIN_STAKE_BY_ROLE,
        unbondingPeriodSecsByRole: UNBONDING_PERIOD_SECS_BY_ROLE,
        slashBps: SLASH_BPS,
        slashingAuthority: admin.publicKey,
        slashDestination: slashDestination.address,
        rewardsAuthority: admin.publicKey,
      })
      .accountsPartial({
        admin: admin.publicKey,
        mint: openMint,
        stakingConfig,
        stakeVault,
        rewardsVault,
        tokenProgram: TOKEN_2022_PROGRAM_ID,
        systemProgram: anchor.web3.SystemProgram.programId,
        rent: SYSVAR_RENT_PUBKEY,
      })
      .rpc({ commitment: "confirmed" });
    console.log("Staking config initialized.");
  }

  console.log("\nVerifying on-chain StakingConfig...");
  const onChain: any = await (program.account as any).stakingConfig.fetch(stakingConfig);

  const assertEq = (label: string, actual: unknown, expected: unknown) => {
    if (String(actual) !== String(expected)) {
      throw new Error(`Assertion failed: ${label} — on-chain ${actual}, expected ${expected}`);
    }
  };
  assertEq("admin", onChain.admin.toBase58(), admin.publicKey.toBase58());
  assertEq("mint", onChain.mint.toBase58(), openMint.toBase58());
  assertEq("slashBps", onChain.slashBps, SLASH_BPS);
  assertEq("slashingAuthority", onChain.slashingAuthority.toBase58(), admin.publicKey.toBase58());
  assertEq(
    "slashDestination",
    onChain.slashDestination.toBase58(),
    slashDestination.address.toBase58(),
  );
  assertEq("rewardsAuthority", onChain.rewardsAuthority.toBase58(), admin.publicKey.toBase58());
  for (let i = 0; i < ROLE_NAMES.length; i++) {
    assertEq(
      `minStakeByRole[${ROLE_NAMES[i]}]`,
      onChain.minStakeByRole[i].toString(),
      MIN_STAKE_BY_ROLE[i]!.toString(),
    );
    assertEq(
      `unbondingPeriodSecsByRole[${ROLE_NAMES[i]}]`,
      onChain.unbondingPeriodSecsByRole[i].toString(),
      UNBONDING_PERIOD_SECS_BY_ROLE[i]!.toString(),
    );
  }
  console.log("On-chain StakingConfig matches expected values.");

  const out = {
    programId: STAKING_PROGRAM_ID.toBase58(),
    openMint: openMint.toBase58(),
    stakingConfig: stakingConfig.toBase58(),
    stakeVault: stakeVault.toBase58(),
    rewardsVault: rewardsVault.toBase58(),
    slashDestination: slashDestination.address.toBase58(),
    slashingAuthority: admin.publicKey.toBase58(),
    rewardsAuthority: admin.publicKey.toBase58(),
    slashBps: SLASH_BPS,
    minStakeByRole: MIN_STAKE_BY_ROLE.map((v) => v.toString()),
    unbondingPeriodSecsByRole: UNBONDING_PERIOD_SECS_BY_ROLE.map((v) => v.toString()),
  };
  allAddresses.devnet_staking = out;
  fs.writeFileSync(addrPath, JSON.stringify(allAddresses, null, 2) + "\n");
  console.log("\nWrote devnet_staking entry to devnet-addresses.json:");
  console.log(JSON.stringify(out, null, 2));
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
