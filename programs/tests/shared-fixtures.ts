// `FeeConfig` and `StakingConfig` are true global singletons (one per
// deployed program, no nonce) — exactly like a real devnet deployment.
// `anchor test` runs every spec file against the same validator/programs
// in one process, so two files each trying to `initialize_fee_config`/
// `initialize_staking_config` independently collide ("already in use").
// These memoized helpers make sure the whole test run initializes each
// singleton exactly once and every file reuses the same instance (and
// the same underlying OPEN mint, since `StakingConfig` is itself
// scoped to one mint).
import * as anchor from "@anchor-lang/core";
import { Program, BN } from "@anchor-lang/core";
import { Escrow } from "../target/types/escrow";
import { Staking } from "../target/types/staking";
import { Governance } from "../target/types/governance";
import {
  TOKEN_2022_PROGRAM_ID,
  createMint,
  getOrCreateAssociatedTokenAccount,
} from "@solana/spl-token";
import {
  Keypair,
  PublicKey,
  SystemProgram,
  SYSVAR_RENT_PUBKEY,
} from "@solana/web3.js";

export const MINT_DECIMALS = 6;
/**
 * The shared `GovernanceConfig`'s vote lock, kept at one second so any
 * suite can carry a proposal past its timelock without waiting.
 *
 * Exported because it is a shared-singleton invariant, not a local
 * detail: a suite that writes a longer lock here must restore this value
 * afterwards, or every later suite executing a proposal waits that long
 * and hangs. `governance.ts`'s `update_governance_config` block restores
 * it for exactly that reason.
 */
export const SHARED_VOTE_LOCK_SECS = new BN(1);
export const unit = (n: number) => new BN(n).mul(new BN(10).pow(new BN(MINT_DECIMALS)));

const provider = anchor.AnchorProvider.env();
anchor.setProvider(provider);
const connection = provider.connection;
const admin = (provider.wallet as anchor.Wallet).payer;

async function ata(mint: PublicKey, owner: PublicKey): Promise<PublicKey> {
  const acc = await getOrCreateAssociatedTokenAccount(
    connection,
    admin,
    mint,
    owner,
    false,
    "confirmed",
    { commitment: "confirmed" },
    TOKEN_2022_PROGRAM_ID,
  );
  return acc.address;
}

let mintPromise: Promise<PublicKey> | null = null;
export function getSharedMint(): Promise<PublicKey> {
  if (!mintPromise) {
    mintPromise = createMint(
      connection,
      admin,
      admin.publicKey,
      null,
      MINT_DECIMALS,
      undefined,
      { commitment: "confirmed" },
      TOKEN_2022_PROGRAM_ID,
    );
  }
  return mintPromise;
}

export interface SharedFeeConfig {
  feeConfig: PublicKey;
  devTreasury: PublicKey;
  ecosystemTreasury: PublicKey;
  infraTreasury: PublicKey;
  emergencyReserve: PublicKey;
}

// A second mint, standing in for OPEN. The settlement stablecoin and OPEN
// are different mints in production, and the dispute path depends on that:
// the arbitration deposit is OPEN-denominated while the trade escrow holds
// the stablecoin, so a merchant's OPEN vault and their settlement vault are
// different accounts. Sharing one mint here would collapse them into one,
// which `execute_dispute_outcome` explicitly rejects.
let openMintPromise: Promise<PublicKey> | null = null;
export function getSharedOpenMint(): Promise<PublicKey> {
  if (!openMintPromise) {
    openMintPromise = createMint(
      connection,
      admin,
      admin.publicKey,
      null,
      MINT_DECIMALS,
      undefined,
      { commitment: "confirmed" },
      TOKEN_2022_PROGRAM_ID,
    );
  }
  return openMintPromise;
}

let arbitrationPoolPromise: Promise<PublicKey> | null = null;
export function getSharedArbitrationPool(escrow: Program<Escrow>): Promise<PublicKey> {
  if (!arbitrationPoolPromise) {
    arbitrationPoolPromise = (async () => {
      const openMint = await getSharedOpenMint();
      const { feeConfig } = await getSharedFeeConfig(escrow);
      const arbitrationPool = PublicKey.findProgramAddressSync(
        [Buffer.from("arbitration_pool")],
        escrow.programId,
      )[0];
      await escrow.methods
        .initializeArbitrationPool()
        .accountsPartial({
          admin: admin.publicKey,
          feeConfig,
          mint: openMint,
          arbitrationPool,
          tokenProgram: TOKEN_2022_PROGRAM_ID,
          systemProgram: SystemProgram.programId,
        })
        .rpc({ commitment: "confirmed" });
      return arbitrationPool;
    })();
  }
  return arbitrationPoolPromise;
}

let feeConfigPromise: Promise<SharedFeeConfig> | null = null;
export function getSharedFeeConfig(escrow: Program<Escrow>): Promise<SharedFeeConfig> {
  if (!feeConfigPromise) {
    feeConfigPromise = (async () => {
      const mint = await getSharedMint();
      const feeConfig = PublicKey.findProgramAddressSync(
        [Buffer.from("fee_config")],
        escrow.programId,
      )[0];
      const devTreasury = await ata(mint, Keypair.generate().publicKey);
      const ecosystemTreasury = await ata(mint, Keypair.generate().publicKey);
      const infraTreasury = await ata(mint, Keypair.generate().publicKey);
      const emergencyReserve = await ata(mint, Keypair.generate().publicKey);

      await escrow.methods
        .initializeFeeConfig({
          adListingFee: new BN(0),
          disputeFilingFee: new BN(0),
          settlementFeeBps: 85,
          devTreasury,
          ecosystemTreasury,
          infraTreasury,
          emergencyReserve,
          devTreasuryBps: 4000,
          ecosystemTreasuryBps: 3000,
          infraTreasuryBps: 2000,
          emergencyReserveBps: 1000,
          timeoutSecs: new BN(1800),
        })
        .accountsPartial({
          admin: admin.publicKey,
          feeConfig,
          systemProgram: SystemProgram.programId,
        })
        .rpc({ commitment: "confirmed" });

      return { feeConfig, devTreasury, ecosystemTreasury, infraTreasury, emergencyReserve };
    })();
  }
  return feeConfigPromise;
}

export interface SharedStakingConfig {
  stakingConfig: PublicKey;
  stakeVault: PublicKey;
  rewardsVault: PublicKey;
  slashingAuthority: Keypair;
  rewardsAuthority: Keypair;
  slashDestination: PublicKey;
}

let stakingConfigPromise: Promise<SharedStakingConfig> | null = null;
export function getSharedStakingConfig(staking: Program<Staking>): Promise<SharedStakingConfig> {
  if (!stakingConfigPromise) {
    stakingConfigPromise = (async () => {
      const mint = await getSharedMint();
      const stakingConfig = PublicKey.findProgramAddressSync(
        [Buffer.from("staking_config")],
        staking.programId,
      )[0];
      const stakeVault = PublicKey.findProgramAddressSync(
        [Buffer.from("stake_vault")],
        staking.programId,
      )[0];
      const rewardsVault = PublicKey.findProgramAddressSync(
        [Buffer.from("rewards_vault")],
        staking.programId,
      )[0];
      const slashingAuthority = Keypair.generate();
      const rewardsAuthority = Keypair.generate();
      const slashDestination = await ata(mint, Keypair.generate().publicKey);

      await staking.methods
        .initializeStakingConfig({
          // Indexed by Role: Merchant, Arbitrator, NodeOperator,
          // NotificationProvider, OracleProvider, RiskIntelligence,
          // SnapshotProvider (OFS-4100 §4 figures).
          minStakeByRole: [
            unit(1000),
            unit(10000),
            unit(1000),
            unit(5000),
            unit(1000),
            unit(1000),
            unit(1000),
          ],
          unbondingPeriodSecs: new BN(1),
          slashBps: 1000,
          slashingAuthority: slashingAuthority.publicKey,
          slashDestination,
          rewardsAuthority: rewardsAuthority.publicKey,
        })
        .accountsPartial({
          admin: admin.publicKey,
          mint,
          stakingConfig,
          stakeVault,
          rewardsVault,
          tokenProgram: TOKEN_2022_PROGRAM_ID,
          systemProgram: SystemProgram.programId,
        })
        .rpc({ commitment: "confirmed" });

      return { stakingConfig, stakeVault, rewardsVault, slashingAuthority, rewardsAuthority, slashDestination };
    })();
  }
  return stakingConfigPromise;
}

export interface SharedGovernanceConfig {
  governanceConfig: PublicKey;
  depositVault: PublicKey;
  forfeitDestination: PublicKey;
  totalOpenSupply: BN;
  quorumBps: number;
  thresholdSimpleBps: number;
  thresholdTreasuryBps: number;
  thresholdUpgradeBps: number;
  quorumUpgradeBps: number;
  depositAmount: BN;
}

/// `GovernanceConfig` is the third global singleton, and became a shared
/// fixture for the same reason the other two are: the ban-list spec file
/// needs the same instance `governance.ts` initializes, and whichever
/// file's `before` ran second would otherwise fail with "already in
/// use". The parameter values are the ones `governance.ts` has always
/// used — moved, not changed.
let governanceConfigPromise: Promise<SharedGovernanceConfig> | null = null;
export function getSharedGovernanceConfig(
  governance: Program<Governance>,
): Promise<SharedGovernanceConfig> {
  if (!governanceConfigPromise) {
    governanceConfigPromise = (async () => {
      const mint = await getSharedMint();
      const governanceConfig = PublicKey.findProgramAddressSync(
        [Buffer.from("governance_config")],
        governance.programId,
      )[0];
      const depositVault = PublicKey.findProgramAddressSync(
        [Buffer.from("deposit_vault")],
        governance.programId,
      )[0];
      const forfeitDestination = await ata(mint, Keypair.generate().publicKey);

      const cfg = {
        totalOpenSupply: unit(1_000_000_000), // OFS-4100 §1
        quorumBps: 1000, // 10%
        thresholdSimpleBps: 5000, // 50%
        thresholdTreasuryBps: 6000, // 60%
        thresholdUpgradeBps: 6600, // 66%
        quorumUpgradeBps: 2000, // 20%
        depositAmount: unit(5000),
      };

      await governance.methods
        .initializeGovernanceConfig({
          ...cfg,
          forfeitDestination,
          voteLockSecs: SHARED_VOTE_LOCK_SECS,
        })
        .accountsPartial({
          admin: admin.publicKey,
          mint,
          governanceConfig,
          depositVault,
          tokenProgram: TOKEN_2022_PROGRAM_ID,
          systemProgram: SystemProgram.programId,
          rent: SYSVAR_RENT_PUBKEY,
        })
        .rpc({ commitment: "confirmed" });

      return { governanceConfig, depositVault, forfeitDestination, ...cfg };
    })();
  }
  return governanceConfigPromise;
}
