/**
 * Proves the OFS-7100 §12 ban list on the real devnet deployment.
 *
 * `anchor test` proves the programs against a fresh local validator with
 * freshly-initialized singletons. That is necessary but not sufficient:
 * it cannot tell you the *deployed* binaries carry the gate, nor that the
 * upgrade left the live `GovernanceConfig` intact. This script asserts
 * both against devnet.
 *
 * What it proves, in order:
 *   1. `GovernanceConfig` still decodes with a balanced field-by-field
 *      walk (OFS-4200 §7.1) — the upgrade disturbed no stored value.
 *   2. `list_wallet` creates a readable `BanRecord` at [b"ban", wallet].
 *   3. The deployed `staking` refuses that wallet's `stake` with
 *      WalletBanned — enforcement crosses the program boundary on chain,
 *      not just in tests.
 *   4. A substituted `ban_record` account is rejected with
 *      ConstraintSeeds — the property the whole design rests on.
 *   5. `delist_wallet` closes the record and access is restored.
 *
 * Devnet only. Costs a few thousand lamports of rent, all reclaimed
 * except the throwaway wallet's funding.
 */
import * as anchor from "@anchor-lang/core";
import { Program, BN } from "@anchor-lang/core";
import { Governance } from "../target/types/governance";
import { Staking } from "../target/types/staking";
import {
  TOKEN_2022_PROGRAM_ID,
  getOrCreateAssociatedTokenAccount,
} from "@solana/spl-token";
import { Keypair, PublicKey, SystemProgram } from "@solana/web3.js";

const OPEN_MINT = new PublicKey("29w8TroBTYoaqrXBDcpv5L54VZRA8Kf7kU5U1cakvFdj");
const ROLE_NODE_OPERATOR = { nodeOperator: {} };

function assert(cond: unknown, msg: string): asserts cond {
  if (!cond) throw new Error(`ASSERTION FAILED: ${msg}`);
}

/**
 * OFS-4200 §7.1: decode with one moving cursor and require the walk to
 * consume exactly `length - trailing bumps`. Hand-computed per-field
 * offsets have silently skipped a field in this repo before, producing
 * two well-formed pubkeys with their identities transposed. A cursor
 * that must balance cannot skip a field without the total disagreeing.
 */
function decodeGovernanceConfig(data: Buffer) {
  let o = 8; // discriminator
  const pubkey = () => {
    const v = new PublicKey(data.subarray(o, o + 32));
    o += 32;
    return v;
  };
  const u64 = () => {
    const v = data.readBigUInt64LE(o);
    o += 8;
    return v;
  };
  const i64 = () => {
    const v = data.readBigInt64LE(o);
    o += 8;
    return v;
  };
  const u16 = () => {
    const v = data.readUInt16LE(o);
    o += 2;
    return v;
  };

  const decoded = {
    admin: pubkey(),
    mint: pubkey(),
    totalOpenSupply: u64(),
    quorumBps: u16(),
    thresholdSimpleBps: u16(),
    thresholdTreasuryBps: u16(),
    thresholdUpgradeBps: u16(),
    quorumUpgradeBps: u16(),
    depositAmount: u64(),
    forfeitDestination: pubkey(),
    voteLockSecs: i64(),
  };
  // Two trailing u8 bumps deliberately left unread — the walk must land
  // exactly there, not merely somewhere plausible.
  assert(
    o === data.length - 2,
    `cursor landed at ${o}, expected ${data.length - 2} (length ${data.length} minus two trailing bumps)`,
  );
  return decoded;
}

async function main() {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const connection = provider.connection;
  const admin = (provider.wallet as anchor.Wallet).payer;

  const governance = anchor.workspace.governance as Program<Governance>;
  const staking = anchor.workspace.staking as Program<Staking>;

  const [governanceConfig] = PublicKey.findProgramAddressSync(
    [Buffer.from("governance_config")],
    governance.programId,
  );
  const [stakingConfig] = PublicKey.findProgramAddressSync(
    [Buffer.from("staking_config")],
    staking.programId,
  );
  const [stakeVault] = PublicKey.findProgramAddressSync(
    [Buffer.from("stake_vault")],
    staking.programId,
  );

  // ---- 1. the upgrade left GovernanceConfig intact -------------------
  const raw = await connection.getAccountInfo(governanceConfig, "confirmed");
  assert(raw, "GovernanceConfig missing on devnet");
  const cfg = decodeGovernanceConfig(raw.data);
  console.log("GovernanceConfig balanced decode OK:", {
    admin: cfg.admin.toBase58(),
    mint: cfg.mint.toBase58(),
    totalOpenSupply: cfg.totalOpenSupply.toString(),
    quorumBps: cfg.quorumBps,
    thresholdSimpleBps: cfg.thresholdSimpleBps,
    thresholdTreasuryBps: cfg.thresholdTreasuryBps,
    thresholdUpgradeBps: cfg.thresholdUpgradeBps,
    quorumUpgradeBps: cfg.quorumUpgradeBps,
    depositAmount: cfg.depositAmount.toString(),
    forfeitDestination: cfg.forfeitDestination.toBase58(),
    voteLockSecs: cfg.voteLockSecs.toString(),
  });
  assert(
    cfg.admin.equals(admin.publicKey),
    `this wallet (${admin.publicKey.toBase58()}) is not the config admin (${cfg.admin.toBase58()})`,
  );

  // ---- a throwaway wallet with a stake account and an OPEN ATA -------
  // Funded by transfer, not by requestAirdrop: the devnet faucet
  // rate-limits and returns an opaque internal error, which would make
  // this script fail for a reason that has nothing to do with the ban
  // list. The admin wallet is already funded to deploy.
  const victim = Keypair.generate();
  {
    const fund = new anchor.web3.Transaction().add(
      SystemProgram.transfer({
        fromPubkey: admin.publicKey,
        toPubkey: victim.publicKey,
        lamports: 30_000_000,
      }),
    );
    await anchor.web3.sendAndConfirmTransaction(connection, fund, [admin], {
      commitment: "confirmed",
    });
  }
  const [stakeAccount] = PublicKey.findProgramAddressSync(
    [Buffer.from("stake"), victim.publicKey.toBuffer(), Buffer.from([2])],
    staking.programId,
  );
  await staking.methods
    .initializeStakeAccount(ROLE_NODE_OPERATOR)
    .accountsPartial({
      owner: victim.publicKey,
      stakeAccount,
      systemProgram: SystemProgram.programId,
    })
    .signers([victim])
    .rpc({ commitment: "confirmed" });
  const victimAta = (
    await getOrCreateAssociatedTokenAccount(
      connection,
      admin,
      OPEN_MINT,
      victim.publicKey,
      false,
      "confirmed",
      { commitment: "confirmed" },
      TOKEN_2022_PROGRAM_ID,
    )
  ).address;
  console.log("throwaway wallet ready:", victim.publicKey.toBase58());

  const stakeIx = () =>
    staking.methods
      .stake(new BN(1))
      .accountsPartial({
        owner: victim.publicKey,
        stakingConfig,
        stakeAccount,
        stakeVault,
        from: victimAta,
        mint: OPEN_MINT,
        tokenProgram: TOKEN_2022_PROGRAM_ID,
      })
      .signers([victim]);

  const [banRecord] = PublicKey.findProgramAddressSync(
    [Buffer.from("ban"), victim.publicKey.toBuffer()],
    governance.programId,
  );

  // ---- 2. list ------------------------------------------------------
  const evidence = [...Buffer.alloc(32, 0xab)];
  const listSig = await governance.methods
    .listWallet(victim.publicKey, { sanctions: {} }, evidence)
    .accountsPartial({
      admin: admin.publicKey,
      governanceConfig,
      banRecord,
      systemProgram: SystemProgram.programId,
    })
    .rpc({ commitment: "confirmed" });
  const record = await governance.account.banRecord.fetch(banRecord);
  assert(record.wallet.equals(victim.publicKey), "BanRecord names the wrong wallet");
  assert([...record.evidenceHash].join() === evidence.join(), "evidence hash mismatch");
  console.log("listed:", listSig, "| record readable at", banRecord.toBase58());

  // ---- 3. the deployed staking program refuses the deposit ----------
  try {
    await stakeIx().rpc({ commitment: "confirmed" });
    throw new Error("ASSERTION FAILED: banned wallet was allowed to stake");
  } catch (err: any) {
    const code = err?.error?.errorCode?.code;
    assert(code === "WalletBanned", `expected WalletBanned, got ${code ?? err}`);
    console.log("staking refused the banned wallet with WalletBanned");
  }

  // ---- 4. a substituted ban_record is rejected ----------------------
  // A real, correctly-derived, genuinely-empty ban PDA — just someone
  // else's. Only the binding to the signer's own key rejects it.
  const [someoneElsesBan] = PublicKey.findProgramAddressSync(
    [Buffer.from("ban"), Keypair.generate().publicKey.toBuffer()],
    governance.programId,
  );
  assert(
    (await connection.getAccountInfo(someoneElsesBan)) === null,
    "the borrowed ban address must be empty for this to be a real attack",
  );
  try {
    await stakeIx()
      .accountsPartial({ banRecord: someoneElsesBan })
      .rpc({ commitment: "confirmed" });
    throw new Error("ASSERTION FAILED: substituted ban_record was accepted");
  } catch (err: any) {
    const code = err?.error?.errorCode?.code;
    assert(code === "ConstraintSeeds", `expected ConstraintSeeds, got ${code ?? err}`);
    console.log("substituted ban_record rejected with ConstraintSeeds");
  }

  // ---- 5. delist restores access ------------------------------------
  const delistSig = await governance.methods
    .delistWallet(victim.publicKey)
    .accountsPartial({ admin: admin.publicKey, governanceConfig, banRecord })
    .rpc({ commitment: "confirmed" });
  assert(
    (await connection.getAccountInfo(banRecord)) === null,
    "BanRecord still exists after delisting",
  );
  console.log("delisted:", delistSig, "| record closed");

  // The wallet holds no OPEN, so a restored-access stake now fails on
  // funds rather than on the ban. That is the point: a *different*
  // failure proves the gate is no longer the thing stopping it.
  try {
    await stakeIx().rpc({ commitment: "confirmed" });
    console.log("post-delist stake succeeded");
  } catch (err: any) {
    const code = err?.error?.errorCode?.code;
    assert(
      code !== "WalletBanned",
      "still WalletBanned after delisting — access was not restored",
    );
    console.log(`post-delist stake no longer hits the ban (failed on ${code ?? "funds"} instead)`);
  }

  console.log("\nALL DEVNET BAN-LIST ASSERTIONS PASSED");
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
