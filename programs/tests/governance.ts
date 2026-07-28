import * as anchor from "@anchor-lang/core";
import { Program, BN } from "@anchor-lang/core";
import { Governance } from "../target/types/governance";
import { Staking } from "../target/types/staking";
import * as crypto from "crypto";
import {
  TOKEN_2022_PROGRAM_ID,
  mintTo,
  getOrCreateAssociatedTokenAccount,
  getAccount,
} from "@solana/spl-token";
import { Keypair, PublicKey, SystemProgram, SYSVAR_RENT_PUBKEY } from "@solana/web3.js";
import { expect } from "chai";
import { getSharedMint, getSharedStakingConfig, unit, MINT_DECIMALS } from "./shared-fixtures";

describe("governance", () => {
  anchor.setProvider(anchor.AnchorProvider.env());
  const provider = anchor.AnchorProvider.env();
  const connection = provider.connection;

  const program = anchor.workspace.governance as Program<Governance>;
  const staking = anchor.workspace.staking as Program<Staking>;
  const admin = (provider.wallet as anchor.Wallet).payer;

  const ROLE_NODE_OPERATOR = { nodeOperator: {} };
  const CATEGORY_PARAMETER = { parameter: {} };
  const CATEGORY_TREASURY = { treasury: {} };

  let mint: PublicKey;
  let governanceConfig: PublicKey;
  let depositVault: PublicKey;
  let stakingConfig: PublicKey;
  let stakeVault: PublicKey;
  let forfeitDestination: PublicKey;

  const TOTAL_OPEN_SUPPLY = unit(1_000_000_000).toString(); // OFS-4100 §1
  const QUORUM_BPS = 1000; // 10%
  const THRESHOLD_SIMPLE_BPS = 5000; // 50%
  const THRESHOLD_TREASURY_BPS = 6000; // 60%
  const THRESHOLD_UPGRADE_BPS = 6600; // 66%
  const QUORUM_UPGRADE_BPS = 2000; // 20%
  const DEPOSIT_AMOUNT = unit(5000);

  async function airdrop(pubkey: PublicKey, sol = 10) {
    const sig = await connection.requestAirdrop(pubkey, sol * 1_000_000_000);
    const latest = await connection.getLatestBlockhash();
    await connection.confirmTransaction({ signature: sig, ...latest });
  }

  async function ata(mintPk: PublicKey, owner: PublicKey) {
    const acc = await getOrCreateAssociatedTokenAccount(
      connection,
      admin,
      mintPk,
      owner,
      false,
      "confirmed",
      { commitment: "confirmed" },
      TOKEN_2022_PROGRAM_ID,
    );
    return acc.address;
  }

  async function mintTokens(dest: PublicKey, amount: BN) {
    await mintTo(
      connection,
      admin,
      mint,
      dest,
      admin,
      BigInt(amount.toString()),
      [],
      { commitment: "confirmed" },
      TOKEN_2022_PROGRAM_ID,
    );
  }

  async function withBlockhashRetry<T>(fn: () => Promise<T>, attempts = 4): Promise<T> {
    for (let i = 0; i < attempts; i++) {
      try {
        return await fn();
      } catch (err) {
        const isBlockhashRace = err instanceof Error && err.message.includes("Blockhash not found");
        if (!isBlockhashRace || i === attempts - 1) throw err;
        await new Promise((r) => setTimeout(r, 250));
      }
    }
    throw new Error("unreachable");
  }

  async function expectAnchorError(p: Promise<unknown>, code: string) {
    try {
      await p;
      expect.fail(`expected instruction to fail with ${code}, but it succeeded`);
    } catch (err: any) {
      const actual = err?.error?.errorCode?.code ?? String(err);
      expect(actual).to.equal(code);
    }
  }

  function proposalPda(id: number) {
    return PublicKey.findProgramAddressSync(
      [Buffer.from("proposal"), new BN(id).toArrayLike(Buffer, "le", 8)],
      program.programId,
    )[0];
  }
  function voteRecordPda(proposal: PublicKey, voter: PublicKey) {
    return PublicKey.findProgramAddressSync(
      [Buffer.from("vote"), proposal.toBuffer(), voter.toBuffer()],
      program.programId,
    )[0];
  }
  function stakeAccountPda(owner: PublicKey, roleByte: number) {
    return PublicKey.findProgramAddressSync(
      [Buffer.from("stake"), owner.toBuffer(), Buffer.from([roleByte])],
      staking.programId,
    )[0];
  }

  async function setUpVoter(stakeAmount: BN): Promise<Keypair> {
    const owner = Keypair.generate();
    await airdrop(owner.publicKey);
    const stakeAccount = stakeAccountPda(owner.publicKey, 2); // NodeOperator

    await withBlockhashRetry(() =>
      staking.methods
        .initializeStakeAccount(ROLE_NODE_OPERATOR)
        .accountsPartial({ owner: owner.publicKey, stakeAccount, systemProgram: SystemProgram.programId })
        .signers([owner])
        .rpc({ commitment: "confirmed" }),
    );

    const ownerAta = await ata(mint, owner.publicKey);
    await mintTokens(ownerAta, stakeAmount);
    await withBlockhashRetry(() =>
      staking.methods
        .stake(stakeAmount)
        .accountsPartial({
          owner: owner.publicKey,
          stakingConfig,
          stakeAccount,
          stakeVault,
          from: ownerAta,
          mint,
          tokenProgram: TOKEN_2022_PROGRAM_ID,
        })
        .signers([owner])
        .rpc({ commitment: "confirmed" }),
    );
    return owner;
  }

  async function createFundedProposal(
    id: number,
    category: any,
    votingPeriodSecs: number,
  ): Promise<{ proposer: Keypair; proposal: PublicKey }> {
    const proposer = Keypair.generate();
    await airdrop(proposer.publicKey);
    const proposerAta = await ata(mint, proposer.publicKey);
    await mintTokens(proposerAta, DEPOSIT_AMOUNT);

    const proposal = proposalPda(id);
    await withBlockhashRetry(() =>
      program.methods
        .createProposal(
          new BN(id),
          category,
          [...crypto.randomBytes(32)],
          [...crypto.randomBytes(32)],
          new BN(votingPeriodSecs),
        )
        .accountsPartial({
          proposer: proposer.publicKey,
          mint,
          governanceConfig,
          depositVault,
          from: proposerAta,
          proposal,
          tokenProgram: TOKEN_2022_PROGRAM_ID,
          systemProgram: SystemProgram.programId,
          rent: SYSVAR_RENT_PUBKEY,
        })
        .signers([proposer])
        .rpc({ commitment: "confirmed" }),
    );
    return { proposer, proposal };
  }

  before(async () => {
    mint = await getSharedMint();
    ({ stakingConfig, stakeVault } = await getSharedStakingConfig(staking));

    governanceConfig = PublicKey.findProgramAddressSync(
      [Buffer.from("governance_config")],
      program.programId,
    )[0];
    depositVault = PublicKey.findProgramAddressSync(
      [Buffer.from("deposit_vault")],
      program.programId,
    )[0];
    forfeitDestination = await ata(mint, Keypair.generate().publicKey);

    await withBlockhashRetry(() =>
      program.methods
        .initializeGovernanceConfig({
          totalOpenSupply: new BN(TOTAL_OPEN_SUPPLY),
          quorumBps: QUORUM_BPS,
          thresholdSimpleBps: THRESHOLD_SIMPLE_BPS,
          thresholdTreasuryBps: THRESHOLD_TREASURY_BPS,
          thresholdUpgradeBps: THRESHOLD_UPGRADE_BPS,
          quorumUpgradeBps: QUORUM_UPGRADE_BPS,
          depositAmount: DEPOSIT_AMOUNT,
          forfeitDestination,
          voteLockSecs: new BN(1),
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
        .rpc({ commitment: "confirmed" }),
    );
  });

  it("creates a proposal, snapshotting its category's quorum/threshold", async () => {
    const { proposal } = await createFundedProposal(1, CATEGORY_PARAMETER, 3);
    const account = await program.account.proposal.fetch(proposal);
    expect(account.state).to.deep.equal({ voting: {} });
    expect(account.thresholdSnapshot).to.equal(THRESHOLD_SIMPLE_BPS);

    // quorum_snapshot = total_supply * quorum_bps / 10_000
    const expectedQuorum = new BN(TOTAL_OPEN_SUPPLY).mul(new BN(QUORUM_BPS)).div(new BN(10_000));
    expect(account.quorumSnapshot.toString()).to.equal(expectedQuorum.toString());
  });

  it("weighs votes by real stake, tallies deterministically, and settles the deposit", async () => {
    // Quorum requires 10% of 1e9 OPEN = 1e8 OPEN cast. Use large voters
    // relative to that so this specific proposal can realistically meet
    // quorum within a test's token budget.
    const quorumTarget = new BN(TOTAL_OPEN_SUPPLY).mul(new BN(QUORUM_BPS)).div(new BN(10_000));
    const forVoterStake = quorumTarget.mul(new BN(6)).div(new BN(10)); // 60% of quorum target
    const againstVoterStake = quorumTarget.mul(new BN(5)).div(new BN(10)); // 50% of quorum target
    // total cast = 110% of quorum target -> quorum met; for-share = 6/11 ≈ 54.5% >= 50% threshold -> Accepted

    const { proposer, proposal } = await createFundedProposal(2, CATEGORY_PARAMETER, 25);

    const forVoter = await setUpVoter(forVoterStake);
    const againstVoter = await setUpVoter(againstVoterStake);

    await withBlockhashRetry(() =>
      program.methods
        .castVote(true, ROLE_NODE_OPERATOR)
        .accountsPartial({
          voter: forVoter.publicKey,
          governanceConfig,
          proposal,
          voterStake: stakeAccountPda(forVoter.publicKey, 2),
          voteRecord: voteRecordPda(proposal, forVoter.publicKey),
          systemProgram: SystemProgram.programId,
        })
        .signers([forVoter])
        .rpc({ commitment: "confirmed" }),
    );

    await withBlockhashRetry(() =>
      program.methods
        .castVote(false, ROLE_NODE_OPERATOR)
        .accountsPartial({
          voter: againstVoter.publicKey,
          governanceConfig,
          proposal,
          voterStake: stakeAccountPda(againstVoter.publicKey, 2),
          voteRecord: voteRecordPda(proposal, againstVoter.publicKey),
          systemProgram: SystemProgram.programId,
        })
        .signers([againstVoter])
        .rpc({ commitment: "confirmed" }),
    );

    // A duplicate vote from the same wallet must fail (VoteRecord's PDA
    // already exists) — the double-vote guard the spec relies on.
    let failed = false;
    try {
      await program.methods
        .castVote(true, ROLE_NODE_OPERATOR)
        .accountsPartial({
          voter: forVoter.publicKey,
          governanceConfig,
          proposal,
          voterStake: stakeAccountPda(forVoter.publicKey, 2),
          voteRecord: voteRecordPda(proposal, forVoter.publicKey),
          systemProgram: SystemProgram.programId,
        })
        .signers([forVoter])
        .rpc({ commitment: "confirmed" });
    } catch {
      failed = true;
    }
    expect(failed).to.equal(true);

    await expectAnchorError(
      program.methods
        .tallyAndFinalize()
        .accountsPartial({ proposal })
        .rpc({ commitment: "confirmed" }),
      "VotingStillOpen",
    );

    await new Promise((r) => setTimeout(r, 27000));

    await withBlockhashRetry(() =>
      program.methods
        .tallyAndFinalize()
        .accountsPartial({ proposal })
        .rpc({ commitment: "confirmed" }),
    );

    const account = await program.account.proposal.fetch(proposal);
    expect(account.quorumMet).to.equal(true);
    expect(account.state).to.deep.equal({ accepted: {} });

    // Deposit refunded to the proposer since quorum was met.
    const proposerAta = await ata(mint, proposer.publicKey);
    await withBlockhashRetry(() =>
      program.methods
        .refundOrForfeitDeposit()
        .accountsPartial({
          mint,
          governanceConfig,
          depositVault,
          proposal,
          proposerTokenAccount: proposerAta,
          forfeitDestination,
          tokenProgram: TOKEN_2022_PROGRAM_ID,
        })
        .rpc({ commitment: "confirmed" }),
    );

    const proposerTokens = await getAccount(connection, proposerAta, "confirmed", TOKEN_2022_PROGRAM_ID);
    expect(proposerTokens.amount.toString()).to.equal(DEPOSIT_AMOUNT.toString());

    // update_config_parameter: only callable once Accepted + Parameter.
    await withBlockhashRetry(() =>
      program.methods
        .updateConfigParameter(PublicKey.default, "settlement_fee_bps", new BN(10))
        .accountsPartial({ proposal })
        .rpc({ commitment: "confirmed" }),
    );
    const finalAccount = await program.account.proposal.fetch(proposal);
    expect(finalAccount.executed).to.equal(true);
  });

  it("forfeits the deposit and rejects the proposal when quorum is missed", async () => {
    const { proposer, proposal } = await createFundedProposal(3, CATEGORY_TREASURY, 20);

    // A single small voter, far below the 10%-of-total-supply quorum.
    const voter = await setUpVoter(unit(1000));
    await withBlockhashRetry(() =>
      program.methods
        .castVote(true, ROLE_NODE_OPERATOR)
        .accountsPartial({
          voter: voter.publicKey,
          governanceConfig,
          proposal,
          voterStake: stakeAccountPda(voter.publicKey, 2),
          voteRecord: voteRecordPda(proposal, voter.publicKey),
          systemProgram: SystemProgram.programId,
        })
        .signers([voter])
        .rpc({ commitment: "confirmed" }),
    );

    await new Promise((r) => setTimeout(r, 22000));

    await withBlockhashRetry(() =>
      program.methods
        .tallyAndFinalize()
        .accountsPartial({ proposal })
        .rpc({ commitment: "confirmed" }),
    );

    const account = await program.account.proposal.fetch(proposal);
    expect(account.quorumMet).to.equal(false);
    expect(account.state).to.deep.equal({ rejected: {} });

    const proposerAta = await ata(mint, proposer.publicKey);
    await withBlockhashRetry(() =>
      program.methods
        .refundOrForfeitDeposit()
        .accountsPartial({
          mint,
          governanceConfig,
          depositVault,
          proposal,
          proposerTokenAccount: proposerAta,
          forfeitDestination,
          tokenProgram: TOKEN_2022_PROGRAM_ID,
        })
        .rpc({ commitment: "confirmed" }),
    );

    // Proposer's own ATA received nothing — the deposit was forfeited.
    const proposerTokens = await getAccount(connection, proposerAta, "confirmed", TOKEN_2022_PROGRAM_ID);
    expect(proposerTokens.amount.toString()).to.equal("0");

    // authorize_treasury_spend requires Accepted, not Rejected.
    await expectAnchorError(
      program.methods
        .authorizeTreasurySpend(forfeitDestination, unit(1))
        .accountsPartial({ proposal })
        .rpc({ commitment: "confirmed" }),
      "ProposalNotAccepted",
    );
  });
});
