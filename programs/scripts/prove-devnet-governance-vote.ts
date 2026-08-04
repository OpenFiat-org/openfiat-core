/**
 * Proves stake-weighted governance voting works end to end on devnet, with a
 * real proposal, a real staked voter, and a real signature.
 *
 * # What was actually in doubt
 *
 * Every piece of this path had been unit- and conformance-tested, but never
 * exercised against devnet with real OPEN, for one blunt reason: the OPEN mint
 * authority is permanently unset, so no wallet could obtain OPEN except
 * through a complete presale cycle, and until `obtain-open-for-protocol-proof.ts`
 * ran there was no signable wallet holding any. "Voting works" was therefore an
 * inference from tests, not an observation. This script makes it an observation.
 *
 * # What it proves, specifically
 *
 * `cast_vote` takes **no weight argument**. The weight recorded is computed
 * on-chain from the voter's own `StakeAccount`, reduced to zero below the
 * role's minimum (`StakeAccount::effective_stake`). So the check that matters
 * is not "did a vote land" but "does the weight the chain recorded equal the
 * voter's real effective stake, independently computed here from the staking
 * config" — a self-reported number has nowhere to enter, and this asserts that
 * the number that did enter is the right one.
 *
 * It also asserts the proposal's own `votes_for` moved by exactly that weight,
 * which is a different claim: a `VoteRecord` holding the right number while the
 * tally it feeds moved by a different one would be a real and silent bug.
 *
 * # The rest of the lifecycle
 *
 * It then waits the voting window out, calls `tally_and_finalize`, and settles
 * the proposer's deposit. Those two steps used to be unreachable here: quorum
 * was 10% of the 1,000,000,000 OPEN supply — 100,000,000 OPEN — against roughly
 * 102,000 staked cluster-wide, so every devnet proposal was unpassable by
 * arithmetic and the accept branch, the threshold comparison and the deposit
 * *refund* path had never run against real state. `set-devnet-governance-quorum.ts`
 * rescaled quorum to 100,000 OPEN, which is what makes the whole lifecycle
 * provable rather than only its first half.
 *
 * The outcome is asserted against independently computed arithmetic rather
 * than merely reported, in both directions: below quorum this must resolve
 * `Rejected` with the deposit forfeited, at or above it `Accepted` with the
 * deposit refunded. Quorum alone decides refund-vs-forfeit — not acceptance.
 *
 * IDEMPOTENT. Picks a fresh proposal id and skips a vote that already exists,
 * so a re-run after an RPC timeout resumes rather than failing.
 *
 * Usage:
 *   VOTER_KEYPAIR=/root/openfiat-node-keys/node0-wallet.json \
 *     npx ts-node scripts/prove-devnet-governance-vote.ts [--commit]
 */
import * as fs from "fs";
import * as path from "path";
import * as crypto from "crypto";
import {
  Connection,
  Keypair,
  PublicKey,
  SYSVAR_RENT_PUBKEY,
} from "@solana/web3.js";
import {
  TOKEN_2022_PROGRAM_ID,
  getAccount,
  getAssociatedTokenAddressSync,
} from "@solana/spl-token";
import * as anchor from "@anchor-lang/core";
import { BN } from "@anchor-lang/core";

const GOVERNANCE_PROGRAM_ID = new PublicKey(
  "AVJfKUjHsizkGGUy8sdz4Xma2hVgmgvgg8GmUMs8E4eE",
);
const STAKING_PROGRAM_ID = new PublicKey(
  "HYEXk8XQukBkZbiYB33JyVefQDxqyCpPudad3wBCyYmx",
);

/** `Role::NodeOperator`. Part of the `StakeAccount` seeds, so a wrong value
 *  derives a different account rather than failing loudly. */
const ROLE_NODE_OPERATOR = 2;
const ROLE_NAMES = [
  "Merchant",
  "Arbitrator",
  "NodeOperator",
  "NotificationProvider",
  "OracleProvider",
  "RiskIntelligenceProvider",
  "SnapshotProvider",
];

/**
 * How long the proposal stays open.
 *
 * Short by default so this script can wait the window out and carry on
 * through `tally_and_finalize` and the deposit settlement — the two steps
 * that were unreachable on devnet until quorum was rescaled (see
 * `set-devnet-governance-quorum.ts`). Raise it with `VOTING_PERIOD_SECS` to
 * leave a proposal open for others to vote on instead.
 */
const VOTING_PERIOD_SECS = Number(process.env.VOTING_PERIOD_SECS ?? 60);
const OPEN_DECIMALS = 9;

const leU64 = (n: number | bigint) => new BN(n.toString()).toArrayLike(Buffer, "le", 8);
const stateName = (s: Record<string, unknown>) => Object.keys(s)[0];
const whole = (units: bigint) =>
  (Number(units) / 10 ** OPEN_DECIMALS).toLocaleString("en-US");
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** The validator's own clock, which is what `voting_ends_at` is compared
 *  against on chain — never `Date.now()`, which drifts from it. */
async function chainTime(connection: Connection): Promise<number> {
  for (let attempt = 0; attempt < 5; attempt++) {
    const slot = await connection.getSlot("confirmed");
    const time = await connection.getBlockTime(slot);
    if (time !== null) return time;
    await sleep(1_000);
  }
  throw new Error("could not read the validator's block time after 5 attempts");
}

async function tokenBalance(connection: Connection, address: PublicKey): Promise<bigint> {
  const info = await connection.getAccountInfo(address, "confirmed");
  if (!info) return 0n;
  return (await getAccount(connection, address, "confirmed", TOKEN_2022_PROGRAM_ID)).amount;
}

interface GovernanceConfigAccount {
  mint: PublicKey;
  forfeitDestination: PublicKey;
  depositAmount: BN;
  quorumBps: number;
  totalOpenSupply: BN;
  depositVaultBump: number;
}
interface ProposalAccount {
  id: BN;
  votesFor: BN;
  votesAgainst: BN;
  state: Record<string, unknown>;
  votingEndsAt: BN;
  quorumSnapshot: BN;
  quorumMet: boolean;
}
interface VoteRecordAccount {
  proposal: PublicKey;
  voter: PublicKey;
  weight: BN;
  inFavor: boolean;
  lockedUntil: BN;
}
interface StakeAccountData {
  owner: PublicKey;
  role: Record<string, unknown>;
  amount: BN;
  unbondingAmount: BN;
}
interface StakingConfigAccount {
  minStakeByRole: BN[];
}

function loadIdl(name: string): anchor.Idl {
  const p = path.join(__dirname, "..", "target", "idl", `${name}.json`);
  if (!fs.existsSync(p)) {
    throw new Error(`no target/idl/${name}.json — run \`anchor build\``);
  }
  return JSON.parse(fs.readFileSync(p, "utf-8"));
}

function loadKeypair(p: string): Keypair {
  return Keypair.fromSecretKey(
    Uint8Array.from(JSON.parse(fs.readFileSync(p, "utf-8"))),
  );
}

async function main() {
  const commit = process.argv.includes("--commit");
  const rpc = process.env.ANCHOR_PROVIDER_URL ?? "https://api.devnet.solana.com";
  if (rpc.includes("mainnet")) {
    throw new Error("devnet-only script; refusing a mainnet endpoint");
  }

  const proposer = loadKeypair(
    process.env.SOLANA_KEYPAIR ||
      path.join(process.env.HOME || "~", ".config/solana/id.json"),
  );
  const voterPath =
    process.env.VOTER_KEYPAIR || "/root/openfiat-node-keys/node0-wallet.json";
  const voter = loadKeypair(voterPath);

  const connection = new Connection(rpc, "confirmed");
  const provider = new anchor.AnchorProvider(
    connection,
    new anchor.Wallet(proposer),
    { commitment: "confirmed" },
  );
  const governance = new anchor.Program(loadIdl("governance"), provider);
  const staking = new anchor.Program(loadIdl("staking"), provider);
  const gov = governance.account as unknown as {
    governanceConfig: { fetch(a: PublicKey): Promise<GovernanceConfigAccount> };
    proposal: { fetch(a: PublicKey): Promise<ProposalAccount> };
    voteRecord: { fetch(a: PublicKey): Promise<VoteRecordAccount> };
  };
  const stk = staking.account as unknown as {
    stakeAccount: { fetch(a: PublicKey): Promise<StakeAccountData> };
    stakingConfig: { fetch(a: PublicKey): Promise<StakingConfigAccount> };
  };

  console.log(`rpc      : ${rpc}`);
  console.log(`proposer : ${proposer.publicKey.toBase58()}`);
  console.log(`voter    : ${voter.publicKey.toBase58()}  (${voterPath})`);
  console.log(commit ? "mode     : COMMIT\n" : "mode     : DRY RUN (pass --commit)\n");

  const [governanceConfigPda] = PublicKey.findProgramAddressSync(
    [Buffer.from("governance_config")],
    GOVERNANCE_PROGRAM_ID,
  );
  const [depositVault] = PublicKey.findProgramAddressSync(
    [Buffer.from("deposit_vault")],
    GOVERNANCE_PROGRAM_ID,
  );
  const [stakingConfigPda] = PublicKey.findProgramAddressSync(
    [Buffer.from("staking_config")],
    STAKING_PROGRAM_ID,
  );
  const [voterStakePda] = PublicKey.findProgramAddressSync(
    [
      Buffer.from("stake"),
      voter.publicKey.toBuffer(),
      Buffer.from([ROLE_NODE_OPERATOR]),
    ],
    STAKING_PROGRAM_ID,
  );
  const [banRecord] = PublicKey.findProgramAddressSync(
    [Buffer.from("ban"), proposer.publicKey.toBuffer()],
    GOVERNANCE_PROGRAM_ID,
  );

  const config = await gov.governanceConfig.fetch(governanceConfigPda);
  const mint = new PublicKey(config.mint);
  const deposit = BigInt(config.depositAmount.toString());
  console.log(`governance config : ${governanceConfigPda.toBase58()}`);
  console.log(`OPEN mint         : ${mint.toBase58()}`);
  console.log(`proposal deposit  : ${whole(deposit)} OPEN`);

  // ---- The voter's real weight, computed here independently -------------
  // This is the number the chain must arrive at on its own. Derived from the
  // same two accounts `effective_stake` reads, but in this process, so that
  // agreement later is a genuine cross-check rather than an echo.
  const stakingConfig = await stk.stakingConfig.fetch(stakingConfigPda);
  const stake = await stk.stakeAccount.fetch(voterStakePda);
  const roleIndex = ROLE_NAMES.findIndex(
    (n) => n.toLowerCase() === stateName(stake.role).toLowerCase(),
  );
  if (roleIndex !== ROLE_NODE_OPERATOR) {
    throw new Error(
      `stake account role is ${stateName(stake.role)}, expected NodeOperator`,
    );
  }
  const amount = BigInt(stake.amount.toString());
  const minForRole = BigInt(stakingConfig.minStakeByRole[roleIndex].toString());
  const expectedWeight = amount >= minForRole ? amount : 0n;
  console.log(`\nvoter stake account : ${voterStakePda.toBase58()}`);
  console.log(`  role      : ${ROLE_NAMES[roleIndex]}`);
  console.log(`  amount    : ${whole(amount)} OPEN`);
  console.log(`  role min  : ${whole(minForRole)} OPEN`);
  console.log(`  EXPECTED effective weight: ${whole(expectedWeight)} OPEN`);
  if (expectedWeight === 0n) {
    throw new Error(
      "voter's effective stake is zero — it is below the role minimum, so a " +
        "vote would carry no weight and prove nothing. Stake more first.",
    );
  }

  // ---- Pick a free proposal id ------------------------------------------
  let proposalId = 1;
  let proposalPda: PublicKey;
  for (;;) {
    [proposalPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("proposal"), leU64(proposalId)],
      GOVERNANCE_PROGRAM_ID,
    );
    if (!(await connection.getAccountInfo(proposalPda))) break;
    proposalId += 1;
  }
  const [proposalAction] = PublicKey.findProgramAddressSync(
    [Buffer.from("proposal_action"), proposalPda.toBuffer()],
    GOVERNANCE_PROGRAM_ID,
  );
  const [voteRecord] = PublicKey.findProgramAddressSync(
    [Buffer.from("vote"), proposalPda.toBuffer(), voter.publicKey.toBuffer()],
    GOVERNANCE_PROGRAM_ID,
  );
  console.log(`\nproposal id  : ${proposalId}`);
  console.log(`proposal pda : ${proposalPda.toBase58()}`);
  console.log(`vote record  : ${voteRecord.toBase58()}`);

  if (!commit) {
    console.log(
      `\nDRY RUN: would create proposal ${proposalId} (deposit ${whole(deposit)} OPEN), ` +
        `then vote with ${whole(expectedWeight)} OPEN of weight. Re-run with --commit.`,
    );
    return;
  }

  // ---- Step 1: create the proposal --------------------------------------
  console.log("\n--- Step 1: create_proposal ---");
  const proposerOpen = getAssociatedTokenAddressSync(
    mint,
    proposer.publicKey,
    false,
    TOKEN_2022_PROGRAM_ID,
  );
  const title = `OpenFiat devnet governance proof #${proposalId}`;
  const summary =
    "Informational proposal created solely to prove stake-weighted voting " +
    "records real on-chain weight. No on-chain action attached.";
  const titleHash = crypto.createHash("sha256").update(title).digest();
  const summaryHash = crypto.createHash("sha256").update(summary).digest();

  const createSig = await governance.methods
    .createProposal(
      new BN(proposalId),
      { informational: {} },
      Array.from(titleHash),
      Array.from(summaryHash),
      new BN(VOTING_PERIOD_SECS),
      { none: {} },
    )
    .accountsPartial({
      proposer: proposer.publicKey,
      banRecord,
      mint,
      governanceConfig: governanceConfigPda,
      depositVault,
      from: proposerOpen,
      proposal: proposalPda,
      proposalAction,
      tokenProgram: TOKEN_2022_PROGRAM_ID,
      systemProgram: anchor.web3.SystemProgram.programId,
      rent: SYSVAR_RENT_PUBKEY,
    })
    .rpc({ commitment: "confirmed" });
  console.log(`create_proposal: ${createSig}`);
  console.log(`  title  : ${title}`);

  const before = await gov.proposal.fetch(proposalPda);
  console.log(
    `  state=${stateName(before.state)} votes_for=${whole(BigInt(before.votesFor.toString()))} ` +
      `quorum_snapshot=${whole(BigInt(before.quorumSnapshot.toString()))} OPEN`,
  );

  // ---- Step 2: cast the vote --------------------------------------------
  console.log("\n--- Step 2: cast_vote ---");
  if (await connection.getAccountInfo(voteRecord)) {
    console.log("vote record already exists — skipping");
  } else {
    const voteSig = await governance.methods
      .castVote(true, { nodeOperator: {} })
      .accountsPartial({
        voter: voter.publicKey,
        governanceConfig: governanceConfigPda,
        proposal: proposalPda,
        stakingConfig: stakingConfigPda,
        voterStake: voterStakePda,
        voteRecord,
        systemProgram: anchor.web3.SystemProgram.programId,
      })
      .signers([voter])
      .rpc({ commitment: "confirmed" });
    console.log(`cast_vote: ${voteSig}`);
  }

  // ---- Step 3: verify ----------------------------------------------------
  console.log("\n--- Step 3: verify what the chain recorded ---");
  const record = await gov.voteRecord.fetch(voteRecord);
  const after = await gov.proposal.fetch(proposalPda);
  const recordedWeight = BigInt(record.weight.toString());
  const votesFor = BigInt(after.votesFor.toString());
  const votesForBefore = BigInt(before.votesFor.toString());

  console.log(`vote_record.voter    : ${new PublicKey(record.voter).toBase58()}`);
  console.log(`vote_record.in_favor : ${record.inFavor}`);
  console.log(`vote_record.weight   : ${whole(recordedWeight)} OPEN`);
  console.log(`proposal.votes_for   : ${whole(votesFor)} OPEN`);

  const failures: string[] = [];
  if (new PublicKey(record.voter).toBase58() !== voter.publicKey.toBase58()) {
    failures.push("vote_record.voter is not the wallet that signed");
  }
  if (recordedWeight !== expectedWeight) {
    failures.push(
      `weight mismatch: chain recorded ${whole(recordedWeight)} OPEN, ` +
        `real effective stake is ${whole(expectedWeight)} OPEN`,
    );
  }
  if (votesFor - votesForBefore !== expectedWeight) {
    failures.push(
      `tally moved by ${whole(votesFor - votesForBefore)} OPEN, ` +
        `expected ${whole(expectedWeight)} OPEN`,
    );
  }
  if (!record.inFavor) failures.push("vote recorded against, expected in favour");

  if (failures.length > 0) {
    for (const f of failures) console.error(`  FAIL: ${f}`);
    throw new Error(`${failures.length} check(s) failed`);
  }

  console.log("\nvote PASS");
  console.log(
    `  the chain independently computed ${whole(recordedWeight)} OPEN of weight from the`,
  );
  console.log("  voter's own StakeAccount, and the tally moved by exactly that.");

  // ---- Step 4: tally ------------------------------------------------------
  const quorum = BigInt(after.quorumSnapshot.toString());
  const endsAt = Number(after.votingEndsAt);
  console.log(`\n--- Step 4: tally_and_finalize ---`);
  console.log(`quorum for this proposal: ${whole(quorum)} OPEN`);
  console.log(`votes cast              : ${whole(votesFor)} OPEN`);
  if (votesFor < quorum) {
    console.log(
      "\nvotes are below quorum, so this proposal will resolve Rejected with the deposit " +
        "forfeited. That is a valid outcome to prove, but it is not the accept path — " +
        "stake more, or lower quorum with set-devnet-governance-quorum.ts, to reach it.",
    );
  }

  for (;;) {
    const now = await chainTime(connection);
    if (now >= endsAt) break;
    console.log(`  waiting out the voting window: ${endsAt - now}s to go`);
    await sleep(10_000);
  }

  const tallySig = await governance.methods
    .tallyAndFinalize()
    .accountsPartial({ proposal: proposalPda })
    .rpc({ commitment: "confirmed" });
  console.log(`tally_and_finalize: ${tallySig}`);

  const tallied = await gov.proposal.fetch(proposalPda);
  console.log(`  state      : ${stateName(tallied.state)}`);
  console.log(`  quorum_met : ${tallied.quorumMet}`);

  const expectedQuorumMet = votesFor >= quorum;
  if (tallied.quorumMet !== expectedQuorumMet) {
    throw new Error(
      `quorum_met is ${tallied.quorumMet} but ${whole(votesFor)} OPEN cast against a ` +
        `${whole(quorum)} OPEN quorum says it should be ${expectedQuorumMet}`,
    );
  }
  // A single unanimous "approve" clears any threshold, so with quorum met the
  // only correct outcome is Accepted. Asserted rather than reported: this is
  // the branch that had never once run on devnet.
  const expectedState = expectedQuorumMet ? "accepted" : "rejected";
  if (stateName(tallied.state) !== expectedState) {
    throw new Error(`proposal is ${stateName(tallied.state)}, expected ${expectedState}`);
  }

  // ---- Step 5: settle the deposit ----------------------------------------
  console.log("\n--- Step 5: refund_or_forfeit_deposit ---");
  const balanceBefore = await tokenBalance(connection, proposerOpen);
  const settleSig = await governance.methods
    .refundOrForfeitDeposit()
    .accountsPartial({
      mint,
      governanceConfig: governanceConfigPda,
      depositVault,
      proposal: proposalPda,
      proposerTokenAccount: proposerOpen,
      forfeitDestination: config.forfeitDestination,
      tokenProgram: TOKEN_2022_PROGRAM_ID,
    })
    .rpc({ commitment: "confirmed" });
  console.log(`refund_or_forfeit_deposit: ${settleSig}`);

  // Quorum alone decides refund-vs-forfeit, not acceptance.
  const balanceAfter = await tokenBalance(connection, proposerOpen);
  const returned = balanceAfter - balanceBefore;
  console.log(`proposer OPEN: ${whole(balanceBefore)} -> ${whole(balanceAfter)}`);
  const expectedReturn = tallied.quorumMet ? deposit : 0n;
  if (returned !== expectedReturn) {
    throw new Error(
      `deposit settlement returned ${whole(returned)} OPEN, expected ${whole(expectedReturn)} ` +
        `(quorum_met=${tallied.quorumMet})`,
    );
  }

  console.log("\nLIFECYCLE PASS");
  console.log(
    `  create -> vote (${whole(recordedWeight)} OPEN of real stake) -> tally (${stateName(tallied.state)}, ` +
      `quorum ${tallied.quorumMet ? "met" : "missed"}) -> deposit ` +
      `${tallied.quorumMet ? "refunded" : "forfeited"}.`,
  );
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
