import * as anchor from "@anchor-lang/core";
import { Program, BN } from "@anchor-lang/core";
import { Governance } from "../target/types/governance";
import { Staking } from "../target/types/staking";
import * as crypto from "crypto";
import {
  TOKEN_2022_PROGRAM_ID,
  mintTo,
  createMint,
  getOrCreateAssociatedTokenAccount,
  getAccount,
} from "@solana/spl-token";
import {
  Keypair,
  PublicKey,
  SystemProgram,
  SYSVAR_RENT_PUBKEY,
} from "@solana/web3.js";
import { expect } from "chai";
import {
  getSharedOpenMint,
  getSharedStakingConfig,
  getSharedGovernanceConfig,
  unit,
  MINT_DECIMALS,
  SHARED_VOTE_LOCK_SECS,
} from "./shared-fixtures";
import { passProposal, sleepUntilChainTime } from "./governance-cycle";

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
  const CATEGORY_STANDARDS = { standards: {} };
  const ACTION_NONE = { none: {} };

  /// OPEN. Stake weighs the votes and backs the proposal deposits, and
  /// `TOTAL_OPEN_SUPPLY` below is the denominator quorum is measured
  /// against — so the deposit token and the staked token have to be the
  /// same one, or the quorum arithmetic compares unrelated units. This
  /// suite never touches a settlement mint.
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
      TOKEN_2022_PROGRAM_ID
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
      TOKEN_2022_PROGRAM_ID
    );
  }

  async function withBlockhashRetry<T>(
    fn: () => Promise<T>,
    attempts = 4
  ): Promise<T> {
    for (let i = 0; i < attempts; i++) {
      try {
        return await fn();
      } catch (err) {
        const isBlockhashRace =
          err instanceof Error && err.message.includes("Blockhash not found");
        if (!isBlockhashRace || i === attempts - 1) throw err;
        await new Promise((r) => setTimeout(r, 250));
      }
    }
    throw new Error("unreachable");
  }

  /// Takes a thunk rather than a promise so the call can be replayed: a
  /// "Blockhash not found" means the transaction never reached the
  /// program, which is indistinguishable from the expected rejection if
  /// you only inspect the error. Retrying through `withBlockhashRetry`
  /// keeps that race from being reported as a passing *or* failing
  /// assertion about program behaviour.
  async function expectAnchorError(fn: () => Promise<unknown>, code: string) {
    try {
      await withBlockhashRetry(fn);
      expect.fail(
        `expected instruction to fail with ${code}, but it succeeded`
      );
    } catch (err: any) {
      const actual = err?.error?.errorCode?.code ?? String(err);
      expect(actual).to.equal(code);
    }
  }

  function proposalPda(id: number) {
    return PublicKey.findProgramAddressSync(
      [Buffer.from("proposal"), new BN(id).toArrayLike(Buffer, "le", 8)],
      program.programId
    )[0];
  }
  function proposalActionPda(proposal: PublicKey) {
    return PublicKey.findProgramAddressSync(
      [Buffer.from("proposal_action"), proposal.toBuffer()],
      program.programId
    )[0];
  }
  function voteRecordPda(proposal: PublicKey, voter: PublicKey) {
    return PublicKey.findProgramAddressSync(
      [Buffer.from("vote"), proposal.toBuffer(), voter.toBuffer()],
      program.programId
    )[0];
  }
  const eventParser = new anchor.EventParser(program.programId, program.coder);

  /**
   * The events a *landed* transaction emitted, decoded from its own logs.
   *
   * Deliberately not `.simulate()`. Anchor's simulate helper calls
   * `Transaction.sign(...signers)` for any extra signers, which resets the
   * signature list and so discards the fee payer's signature, and then
   * asks the validator to verify signatures — every builder carrying a
   * `.signers([...])` fails to simulate at all, for reasons that have
   * nothing to do with the program. Reading the transaction that actually
   * executed is the stronger check anyway: it asserts on what was emitted,
   * not on what would have been.
   */
  async function eventsOf(signature: string): Promise<any[]> {
    const tx = await connection.getTransaction(signature, {
      commitment: "confirmed",
      maxSupportedTransactionVersion: 0,
    });
    const logs = tx?.meta?.logMessages;
    if (!logs) throw new Error(`no log messages for ${signature}`);
    return [...eventParser.parseLogs(logs)];
  }

  function stakeAccountPda(owner: PublicKey, roleByte: number) {
    return PublicKey.findProgramAddressSync(
      [Buffer.from("stake"), owner.toBuffer(), Buffer.from([roleByte])],
      staking.programId
    )[0];
  }

  async function setUpVoter(stakeAmount: BN): Promise<Keypair> {
    const owner = Keypair.generate();
    await airdrop(owner.publicKey);
    const stakeAccount = stakeAccountPda(owner.publicKey, 2); // NodeOperator

    await withBlockhashRetry(() =>
      staking.methods
        .initializeStakeAccount(ROLE_NODE_OPERATOR)
        .accountsPartial({
          owner: owner.publicKey,
          stakeAccount,
          systemProgram: SystemProgram.programId,
        })
        .signers([owner])
        .rpc({ commitment: "confirmed" })
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
        .rpc({ commitment: "confirmed" })
    );
    return owner;
  }

  /// Every proposal carries a `GovernanceAction`, fixed at creation.
  /// These ones carry `None`: `update_config_parameter` and
  /// `authorize_treasury_spend` are still record-only, so there is no
  /// action for them to perform. The ban-list actions, which are the
  /// ones that do something, are exercised in `ban-list.ts`.
  async function createFundedProposal(
    id: number,
    category: any,
    votingPeriodSecs: number,
    action: any = ACTION_NONE
  ): Promise<{
    proposer: Keypair;
    proposal: PublicKey;
    events: readonly any[];
  }> {
    const proposer = Keypair.generate();
    await airdrop(proposer.publicKey);
    const proposerAta = await ata(mint, proposer.publicKey);
    await mintTokens(proposerAta, DEPOSIT_AMOUNT);

    const proposal = proposalPda(id);
    // Hoisted out of the builder: it is called once per send attempt, and
    // fresh randomness per attempt would mean a retry landed different
    // content from the one whose event is asserted on below.
    const titleHash = [...crypto.randomBytes(32)];
    const summaryHash = [...crypto.randomBytes(32)];
    const builder = () =>
      program.methods
        .createProposal(
          new BN(id),
          category,
          titleHash,
          summaryHash,
          new BN(votingPeriodSecs),
          action
        )
        .accountsPartial({
          proposer: proposer.publicKey,
          mint,
          governanceConfig,
          depositVault,
          from: proposerAta,
          proposal,
          proposalAction: proposalActionPda(proposal),
          tokenProgram: TOKEN_2022_PROGRAM_ID,
          systemProgram: SystemProgram.programId,
          rent: SYSVAR_RENT_PUBKEY,
        })
        .signers([proposer]);

    const signature = await withBlockhashRetry(() =>
      builder().rpc({ commitment: "confirmed" })
    );
    return { proposer, proposal, events: await eventsOf(signature) };
  }

  before(async () => {
    mint = await getSharedOpenMint();
    ({ stakingConfig, stakeVault } = await getSharedStakingConfig(staking));
    ({ governanceConfig, depositVault, forfeitDestination } =
      await getSharedGovernanceConfig(program));
  });

  it("creates a proposal, snapshotting its category's quorum/threshold", async () => {
    const { proposal, proposer, events } = await createFundedProposal(
      1,
      CATEGORY_PARAMETER,
      3
    );
    const account = await program.account.proposal.fetch(proposal);
    expect(account.state).to.deep.equal({ voting: {} });
    expect(account.thresholdSnapshot).to.equal(THRESHOLD_SIMPLE_BPS);

    // quorum_snapshot = total_supply * quorum_bps / 10_000
    const expectedQuorum = new BN(TOTAL_OPEN_SUPPLY)
      .mul(new BN(QUORUM_BPS))
      .div(new BN(10_000));
    expect(account.quorumSnapshot.toString()).to.equal(
      expectedQuorum.toString()
    );

    // OFS-4100 §9.4: the creation event has to carry the bar the proposal
    // must clear, not just its identity. Both snapshots are fixed now and
    // never change, so this event alone tells a reader what it would take
    // to pass — without it they would have to guess which config values
    // were live at the moment of creation.
    const created = events.find((e: any) => e.name === "proposalCreated");
    expect(created, "proposalCreated event").to.not.be.undefined;
    expect(created.data.proposal.toBase58()).to.equal(proposal.toBase58());
    expect(created.data.proposalId.toString()).to.equal("1");
    expect(created.data.proposer.toBase58()).to.equal(proposer.publicKey.toBase58());
    expect(created.data.category).to.deep.equal(CATEGORY_PARAMETER);
    expect([...created.data.titleHash]).to.deep.equal(
      [...(await program.account.proposal.fetch(proposal)).titleHash]
    );
    expect(created.data.action).to.deep.equal(ACTION_NONE);
    expect(created.data.thresholdSnapshot).to.equal(THRESHOLD_SIMPLE_BPS);
    expect(created.data.quorumSnapshot.toString()).to.equal(expectedQuorum.toString());
    expect(created.data.stakeDeposit.toString()).to.equal(DEPOSIT_AMOUNT.toString());
    expect(created.data.votingEndsAt.toNumber()).to.be.greaterThan(
      created.data.timestamp.toNumber()
    );
  });

  it("fixes what a proposal may do at creation, in its own immutable account", async () => {
    // A proposal that could have its action attached or changed after
    // voting opened would let a proposer collect votes on one thing and
    // spend them on another. The action account is `init`, created in
    // the same instruction as the proposal, so the first voter and the
    // executor see the same one.
    const wallet = Keypair.generate().publicKey;
    const evidence = [...crypto.randomBytes(32)];
    const { proposal, events } = await createFundedProposal(
      11,
      CATEGORY_STANDARDS,
      3,
      { listWallet: { wallet, reason: { sanctions: {} }, evidenceHash: evidence } }
    );

    const action = await program.account.proposalAction.fetch(
      proposalActionPda(proposal)
    );
    expect(action.proposal.toBase58()).to.equal(proposal.toBase58());
    expect(action.action.listWallet.wallet.toBase58()).to.equal(
      wallet.toBase58()
    );
    expect([...action.action.listWallet.evidenceHash]).to.deep.equal(evidence);

    // The same action is in the creation event, in full. A reader deciding
    // how to vote on an exclusion must not have to fetch a second account
    // to learn whom it excludes (OFS-7100 §12.2).
    const created = events.find((e: any) => e.name === "proposalCreated");
    expect(created.data.action.listWallet.wallet.toBase58()).to.equal(
      wallet.toBase58()
    );
    expect([...created.data.action.listWallet.evidenceHash]).to.deep.equal(evidence);
    expect(created.data.proposalAction.toBase58()).to.equal(
      proposalActionPda(proposal).toBase58()
    );

    // There is no instruction that writes it again — creating it a
    // second time is the only way to try, and the address is taken.
    let failed = false;
    try {
      await createFundedProposal(11, CATEGORY_STANDARDS, 3, ACTION_NONE);
    } catch {
      failed = true;
    }
    expect(failed, "a proposal's action must not be replaceable").to.equal(true);
  });

  it("weighs votes by real stake, tallies deterministically, and settles the deposit", async () => {
    // Quorum requires 10% of 1e9 OPEN = 1e8 OPEN cast. Use large voters
    // relative to that so this specific proposal can realistically meet
    // quorum within a test's token budget.
    const quorumTarget = new BN(TOTAL_OPEN_SUPPLY)
      .mul(new BN(QUORUM_BPS))
      .div(new BN(10_000));
    const forVoterStake = quorumTarget.mul(new BN(6)).div(new BN(10)); // 60% of quorum target
    const againstVoterStake = quorumTarget.mul(new BN(5)).div(new BN(10)); // 50% of quorum target
    // total cast = 110% of quorum target -> quorum met; for-share = 6/11 ≈ 54.5% >= 50% threshold -> Accepted

    const { proposer, proposal } = await createFundedProposal(
      2,
      CATEGORY_PARAMETER,
      25
    );

    const forVoter = await setUpVoter(forVoterStake);
    const againstVoter = await setUpVoter(againstVoterStake);

    const forVote = () =>
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
        .signers([forVoter]);

    const forVoteSignature = await withBlockhashRetry(() =>
      forVote().rpc({ commitment: "confirmed" })
    );

    // OFS-4100 §9.4. The assertion that matters is `weight`: `cast_vote`
    // takes no weight argument at all, so this number can only have come
    // from the voter's on-chain StakeAccount via `effective_stake`. An
    // event echoing a self-reported figure is exactly what `crates/rpc`'s
    // async vote verification exists to override, and would hand every
    // indexer a tally the chain does not agree with.
    const voteEvent = (await eventsOf(forVoteSignature)).find(
      (e: any) => e.name === "voteCast"
    );
    expect(voteEvent, "voteCast event").to.not.be.undefined;
    expect(voteEvent.data.weight.toString()).to.equal(forVoterStake.toString());
    expect(voteEvent.data.voter.toBase58()).to.equal(forVoter.publicKey.toBase58());
    expect(voteEvent.data.voterStake.toBase58()).to.equal(
      stakeAccountPda(forVoter.publicKey, 2).toBase58()
    );
    expect(voteEvent.data.role).to.deep.equal(ROLE_NODE_OPERATOR);
    expect(voteEvent.data.inFavor).to.equal(true);
    expect(voteEvent.data.proposal.toBase58()).to.equal(proposal.toBase58());
    // Running total after this vote, so a replay needs no re-summing.
    expect(voteEvent.data.votesFor.toString()).to.equal(forVoterStake.toString());

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
        .rpc({ commitment: "confirmed" })
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
      () =>
        program.methods
          .tallyAndFinalize()
          .accountsPartial({ proposal })
          .rpc({ commitment: "confirmed" }),
      "VotingStillOpen"
    );

    await new Promise((r) => setTimeout(r, 27000));

    const tallySignature = await withBlockhashRetry(() =>
      program.methods
        .tallyAndFinalize()
        .accountsPartial({ proposal })
        .rpc({ commitment: "confirmed" })
    );

    // The tally event must carry the weights AND quorum_met, not just the
    // verdict: quorum alone decides refund versus forfeiture below, so an
    // observer who cannot see it cannot check that the deposit was handled
    // correctly. Everything needed to recompute the decision is here.
    const finalizedEvent = (await eventsOf(tallySignature)).find(
      (e: any) => e.name === "proposalFinalized"
    );
    expect(finalizedEvent, "proposalFinalized event").to.not.be.undefined;
    expect(finalizedEvent.data.quorumMet).to.equal(true);
    expect(finalizedEvent.data.state).to.deep.equal({ accepted: {} });
    expect(finalizedEvent.data.votesFor.toString()).to.equal(forVoterStake.toString());
    expect(finalizedEvent.data.votesAgainst.toString()).to.equal(
      againstVoterStake.toString()
    );
    expect(finalizedEvent.data.totalCast.toString()).to.equal(
      forVoterStake.add(againstVoterStake).toString()
    );
    expect(finalizedEvent.data.totalCast.gte(finalizedEvent.data.quorumSnapshot)).to.equal(
      true
    );

    const account = await program.account.proposal.fetch(proposal);
    expect(account.quorumMet).to.equal(true);
    expect(account.state).to.deep.equal({ accepted: {} });

    // Deposit refunded to the proposer since quorum was met.
    const proposerAta = await ata(mint, proposer.publicKey);
    const settleSignature = await withBlockhashRetry(() =>
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
        .rpc({ commitment: "confirmed" })
    );

    const settledEvent = (await eventsOf(settleSignature)).find(
      (e: any) => e.name === "proposalDepositSettled"
    );
    expect(settledEvent, "proposalDepositSettled event").to.not.be.undefined;
    expect(settledEvent.data.refunded).to.equal(true);
    expect(settledEvent.data.quorumMet).to.equal(true);
    expect(settledEvent.data.destination.toBase58()).to.equal(proposerAta.toBase58());
    expect(settledEvent.data.amount.toString()).to.equal(DEPOSIT_AMOUNT.toString());

    const proposerTokens = await getAccount(
      connection,
      proposerAta,
      "confirmed",
      TOKEN_2022_PROGRAM_ID
    );
    expect(proposerTokens.amount.toString()).to.equal(
      DEPOSIT_AMOUNT.toString()
    );

    // update_config_parameter: only callable once Accepted + Parameter.
    const authorizeParameter = () =>
      program.methods
        .updateConfigParameter(
          PublicKey.default,
          "settlement_fee_bps",
          new BN(10)
        )
        .accountsPartial({ proposal });

    const paramSignature = await withBlockhashRetry(() =>
      authorizeParameter().rpc({ commitment: "confirmed" })
    );

    // Record-only: this instruction writes no parameter anywhere. The
    // event has to say so — hence `authorizedValue`, not `newValue`.
    const paramEvent = (await eventsOf(paramSignature)).find(
      (e: any) => e.name === "configParameterChangeAuthorized"
    );
    expect(paramEvent, "configParameterChangeAuthorized event").to.not.be.undefined;
    expect(paramEvent.data.parameterKey).to.equal("settlement_fee_bps");
    expect(paramEvent.data.authorizedValue.toString()).to.equal("10");
    expect(paramEvent.data.targetProgram.toBase58()).to.equal(
      PublicKey.default.toBase58()
    );
    expect(paramEvent.data.proposal.toBase58()).to.equal(proposal.toBase58());
    expect(Object.keys(paramEvent.data)).to.not.include("applied");

    const finalAccount = await program.account.proposal.fetch(proposal);
    expect(finalAccount.executed).to.equal(true);
  });

  it("forfeits the deposit and rejects the proposal when quorum is missed", async () => {
    const { proposer, proposal } = await createFundedProposal(
      3,
      CATEGORY_TREASURY,
      20
    );

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
        .rpc({ commitment: "confirmed" })
    );

    await new Promise((r) => setTimeout(r, 22000));

    await withBlockhashRetry(() =>
      program.methods
        .tallyAndFinalize()
        .accountsPartial({ proposal })
        .rpc({ commitment: "confirmed" })
    );

    const account = await program.account.proposal.fetch(proposal);
    expect(account.quorumMet).to.equal(false);
    expect(account.state).to.deep.equal({ rejected: {} });

    const proposerAta = await ata(mint, proposer.publicKey);
    const settleSignature = await withBlockhashRetry(() =>
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
        .rpc({ commitment: "confirmed" })
    );

    // The other branch of the same event. `refunded` and `quorumMet`
    // disagree with the accepted case above and agree with each other,
    // which is the whole point of carrying both: a forfeiture nobody can
    // check against the tally is indistinguishable from a confiscation.
    const settledEvent = (await eventsOf(settleSignature)).find(
      (e: any) => e.name === "proposalDepositSettled"
    );
    expect(settledEvent, "proposalDepositSettled event").to.not.be.undefined;
    expect(settledEvent.data.refunded).to.equal(false);
    expect(settledEvent.data.quorumMet).to.equal(false);
    expect(settledEvent.data.state).to.deep.equal({ rejected: {} });
    expect(settledEvent.data.destination.toBase58()).to.equal(
      forfeitDestination.toBase58()
    );

    // Proposer's own ATA received nothing — the deposit was forfeited.
    const proposerTokens = await getAccount(
      connection,
      proposerAta,
      "confirmed",
      TOKEN_2022_PROGRAM_ID
    );
    expect(proposerTokens.amount.toString()).to.equal("0");

    // authorize_treasury_spend requires Accepted, not Rejected.
    await expectAnchorError(
      () =>
        program.methods
          .authorizeTreasurySpend(forfeitDestination, unit(1))
          .accountsPartial({ proposal })
          .rpc({ commitment: "confirmed" }),
      "ProposalNotAccepted"
    );
  });

  it("records a treasury spend as authorized, and moves nothing", async () => {
    // The one governance instruction whose event could most easily lie.
    // `authorize_treasury_spend` sets `executed` and returns — governance
    // holds no treasury to disburse from (OFS-4200 §1) — so the event has
    // to read as an authorization or every explorer reports protocol funds
    // leaving an account they are still sitting in.
    const proposal = await passProposal(
      program,
      staking,
      ACTION_NONE,
      CATEGORY_TREASURY
    );
    const amount = unit(1);
    const before = await getAccount(
      connection,
      forfeitDestination,
      "confirmed",
      TOKEN_2022_PROGRAM_ID
    );

    const signature = await withBlockhashRetry(() =>
      program.methods
        .authorizeTreasurySpend(forfeitDestination, amount)
        .accountsPartial({ proposal })
        .rpc({ commitment: "confirmed" })
    );

    const event = (await eventsOf(signature)).find(
      (e: any) => e.name === "treasurySpendAuthorized"
    );
    expect(event, "treasurySpendAuthorized event").to.not.be.undefined;
    expect(event.data.proposal.toBase58()).to.equal(proposal.toBase58());
    expect(event.data.authorizedDestination.toBase58()).to.equal(
      forfeitDestination.toBase58()
    );
    expect(event.data.authorizedAmount.toString()).to.equal(amount.toString());
    // Nothing in the event claims a completed transfer.
    expect(Object.keys(event.data)).to.not.include("disbursed");

    // And the authorization really was only that.
    const after = await getAccount(
      connection,
      forfeitDestination,
      "confirmed",
      TOKEN_2022_PROGRAM_ID
    );
    expect(after.amount.toString()).to.equal(before.amount.toString());
    const account = await program.account.proposal.fetch(proposal);
    expect(account.executed).to.equal(true);
  });

  // OFS-4100 §5.1 — "The sunset must be non-extendable, or it is not a
  // sunset. It has to be enforced on-chain against a timestamp fixed at
  // initialization and immutable afterwards. The holder must not be able
  // to move its own deadline, directly or through any configuration field
  // it can write."
  //
  // These specs are that sentence, tested. The clock cannot be advanced a
  // year on a validator whose time tracks wall time, so the branch *past*
  // the deadline is proven by the program crate's own unit tests
  // (`shared_logic::tests`, which call the same gate the instructions
  // call, at and after expiry). What is proven here is the harder and
  // more important half: that nothing can move the deadline in the first
  // place.
  describe("AllenHark's first-year exception and its sunset", () => {
    const ONE_YEAR_SECS = 365 * 24 * 60 * 60;
    const emergencyAuthorityPda = PublicKey.findProgramAddressSync(
      [Buffer.from("emergency_authority")],
      program.programId
    )[0];

    it("fixes the deadline at exactly one year from initialization", async () => {
      const authority = await program.account.emergencyAuthority.fetch(
        emergencyAuthorityPda
      );
      expect(
        authority.expiresAt.sub(authority.initializedAt).toNumber(),
        "the window must be one year wide, not one year plus whatever the caller asked for"
      ).to.equal(ONE_YEAR_SECS);
    });

    it("records both AllenHark keys as equal first-class holders", async () => {
      // §5.1: "Both addresses are first-class authorities and must be
      // presented as such in every application, explorer and document —
      // not one with the other as a footnote." Recorded on the account
      // rather than left implicit in the program binary so an explorer
      // can read them rather than trust a document.
      const authority = await program.account.emergencyAuthority.fetch(
        emergencyAuthorityPda
      );
      expect(authority.primaryHolder.toBase58()).to.equal(
        "ALLENLMtV1zEAHT3xpVryqcbdPCB8c9JhM1Jdbe5XHg5"
      );
      expect(authority.secondaryHolder.toBase58()).to.equal(
        "A11ENCKCBxZxEbXQmqs6mTmJkP8gjcA7xqfLD5BxfRpp"
      );
    });

    it("cannot be re-initialized, so the deadline cannot be re-based", async () => {
      // The most obvious extension attack: run the creation instruction
      // again later and get a deadline a year from *now*. `init` refuses
      // an account that already exists, so the second call cannot land.
      // Asserted against the raw error because "already in use" is a
      // runtime allocation failure, not an Anchor error code.
      const before = await program.account.emergencyAuthority.fetch(
        emergencyAuthorityPda
      );
      let failed = false;
      try {
        await withBlockhashRetry(() =>
          program.methods
            .initializeEmergencyAuthority()
            .accountsPartial({
              payer: admin.publicKey,
              emergencyAuthority: emergencyAuthorityPda,
              systemProgram: SystemProgram.programId,
            })
            .rpc({ commitment: "confirmed" })
        );
      } catch (err: any) {
        failed = true;
        expect(String(err)).to.match(/already in use|custom program error: 0x0/);
      }
      expect(failed, "re-initialization must not succeed").to.equal(true);

      const after = await program.account.emergencyAuthority.fetch(
        emergencyAuthorityPda
      );
      expect(after.expiresAt.toString()).to.equal(before.expiresAt.toString());
    });

    it("cannot be re-created through initialize_governance_config either", async () => {
      // The second creation path, and the same attack through it. Both
      // `init` the same PDA, so whichever ran first wins permanently —
      // there is no ordering that yields two deadlines.
      let failed = false;
      try {
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
              voteLockSecs: new BN(ONE_YEAR_SECS),
            })
            .accountsPartial({
              admin: admin.publicKey,
              mint,
              governanceConfig,
              depositVault,
              emergencyAuthority: emergencyAuthorityPda,
              tokenProgram: TOKEN_2022_PROGRAM_ID,
              systemProgram: SystemProgram.programId,
              rent: SYSVAR_RENT_PUBKEY,
            })
            .rpc({ commitment: "confirmed" })
        );
      } catch (err: any) {
        failed = true;
      }
      expect(failed, "the config's creation path must not re-create it").to.equal(
        true
      );
    });

    it("exposes no instruction that can write the deadline", async () => {
      // The structural proof, and the one that keeps holding as the
      // program grows. Every other test here attacks a path that exists
      // today; this asserts that no *future* instruction can quietly
      // acquire write access to the account without someone changing this
      // list on purpose. A sunset a later commit can make mutable is not
      // a sunset.
      // Anchor's TypeScript client re-cases IDL names, so both spellings
      // are accepted rather than pinning the test to whichever the client
      // version in use happens to emit.
      const canonical = (name: string) =>
        name.replace(/[A-Z]/g, (ch) => `_${ch.toLowerCase()}`);
      const writers = (program.idl as any).instructions
        .filter((ix: any) =>
          ix.accounts.some(
            (acc: any) =>
              canonical(acc.name) === "emergency_authority" && acc.writable
          )
        )
        .map((ix: any) => canonical(ix.name))
        .sort();
      expect(writers).to.deep.equal([
        "initialize_emergency_authority",
        "initialize_governance_config",
      ]);

      // And neither of them takes a duration or a deadline, so even the
      // two instructions that *can* write it cannot be told what to
      // write. The only thing a caller influences is when the clock
      // starts — which can only bring the deadline nearer.
      for (const name of writers) {
        const ix = (program.idl as any).instructions.find(
          (candidate: any) => canonical(candidate.name) === name
        );
        const argNames = JSON.stringify(ix.args);
        expect(argNames).to.not.match(/expires|deadline|duration|secs/i);
      }
    });

    it("does not let a governance vote reach the deadline", async () => {
      // The specific bypass §5.1 calls out: "The holder must not be able
      // to move its own deadline... through any configuration field it
      // can write." A passed proposal acts through `GovernanceAction`, so
      // if none of its variants names the emergency authority, no vote —
      // however overwhelming — can postpone the sunset.
      const action = (program.idl as any).types.find(
        (ty: any) => ty.name === "GovernanceAction"
      );
      expect(JSON.stringify(action)).to.not.match(/emergency|expires/i);
    });

    it("lets vote_lock_secs still be written inside the window", async () => {
      // The control. The sunset must close a power that actually works
      // beforehand — a test suite proving only that something is refused
      // proves nothing about what was taken away. The window is a year
      // wide and this run is inside it, so the write succeeds.
      const before = await program.account.governanceConfig.fetch(
        governanceConfig
      );
      const params = {
        totalOpenSupply: before.totalOpenSupply,
        quorumBps: before.quorumBps,
        thresholdSimpleBps: before.thresholdSimpleBps,
        thresholdTreasuryBps: before.thresholdTreasuryBps,
        thresholdUpgradeBps: before.thresholdUpgradeBps,
        quorumUpgradeBps: before.quorumUpgradeBps,
        depositAmount: before.depositAmount,
        voteLockSecs: new BN(11),
      };
      await withBlockhashRetry(() =>
        program.methods
          .updateGovernanceConfig(params)
          .accountsPartial({
            admin: admin.publicKey,
            governanceConfig,
            mint,
            forfeitDestination: before.forfeitDestination,
          })
          .rpc({ commitment: "confirmed" })
      );
      expect(
        (
          await program.account.governanceConfig.fetch(governanceConfig)
        ).voteLockSecs.toNumber()
      ).to.equal(11);

      // Restore, so the shared config the other suites depend on is
      // exactly as they left it.
      await withBlockhashRetry(() =>
        program.methods
          .updateGovernanceConfig({
            ...params,
            voteLockSecs: before.voteLockSecs,
          })
          .accountsPartial({
            admin: admin.publicKey,
            governanceConfig,
            mint,
            forfeitDestination: before.forfeitDestination,
          })
          .rpc({ commitment: "confirmed" })
      );
    });
  });

  // OFS-4200 §6 / the off-chain governance layer. Nothing correlated a
  // gossiped proposal with an on-chain one, so an interface showing "the"
  // proposal could not tell whether the chain agreed. This is the chain's
  // half of the join.
  describe("link_offchain_proposal", () => {
    // sha256("ofip-0001"), the digest `openfiat_governance::onchain::
    // offchain_id_hash` produces for that id. Written out rather than
    // computed inline so this test and the Rust one are pinned to the
    // same literal — a client hashing differently would write a link no
    // node ever matches.
    const OFIP_0001_HASH = [
      ...Buffer.from(
        "4298e38755cdf4009f0a9beb84960a10c0705ca506826fc77eb8cfd1a2b40ef1",
        "hex"
      ),
    ];

    it("agrees with the digest the node crate computes for the same id", async () => {
      expect(
        [...crypto.createHash("sha256").update("ofip-0001").digest()]
      ).to.deep.equal(OFIP_0001_HASH);
    });

    it("starts unlinked, and records the link the proposer sets", async () => {
      const id = 4101;
      const { proposer, proposal } = await createFundedProposal(
        id,
        CATEGORY_PARAMETER,
        30
      );
      const before = await program.account.proposal.fetch(proposal);
      expect(
        before.offchainIdHash.every((byte: number) => byte === 0),
        "an unlinked proposal must read as all zeroes, the 'none claimed' sentinel"
      ).to.equal(true);

      await withBlockhashRetry(() =>
        program.methods
          .linkOffchainProposal(OFIP_0001_HASH)
          .accountsPartial({ proposer: proposer.publicKey, proposal })
          .signers([proposer])
          .rpc({ commitment: "confirmed" })
      );
      const after = await program.account.proposal.fetch(proposal);
      expect(after.offchainIdHash).to.deep.equal(OFIP_0001_HASH);
    });

    it("refuses a second link, so the claim a voter saw is the claim a reader gets", async () => {
      const id = 4102;
      const { proposer, proposal } = await createFundedProposal(
        id,
        CATEGORY_PARAMETER,
        30
      );
      const link = (hash: number[]) =>
        program.methods
          .linkOffchainProposal(hash)
          .accountsPartial({ proposer: proposer.publicKey, proposal })
          .signers([proposer]);

      await withBlockhashRetry(() => link(OFIP_0001_HASH).rpc({ commitment: "confirmed" }));
      await expectAnchorError(
        () => link([...crypto.randomBytes(32)]).rpc({ commitment: "confirmed" }),
        "OffchainLinkAlreadySet"
      );
      expect(
        (await program.account.proposal.fetch(proposal)).offchainIdHash
      ).to.deep.equal(OFIP_0001_HASH);
    });

    it("refuses anyone but the proposal's own proposer", async () => {
      // Execution instructions in this program are deliberately
      // permissionless — the authority is the vote. This is not one: a
      // stranger attaching a claim would both misattribute the proposal
      // and spend its single write.
      const id = 4103;
      const { proposal } = await createFundedProposal(id, CATEGORY_PARAMETER, 30);
      const stranger = Keypair.generate();
      await airdrop(stranger.publicKey);
      await expectAnchorError(
        () =>
          program.methods
            .linkOffchainProposal(OFIP_0001_HASH)
            .accountsPartial({ proposer: stranger.publicKey, proposal })
            .signers([stranger])
            .rpc({ commitment: "confirmed" }),
        "NotTheProposer"
      );
    });

    it("refuses an all-zero digest, which would read as unlinked", async () => {
      // All zeroes is the sentinel. Storing it would consume the one-shot
      // write while leaving the proposal looking unlinked forever, so
      // "unset" and "set to nothing" must stay indistinguishable.
      const id = 4104;
      const { proposer, proposal } = await createFundedProposal(
        id,
        CATEGORY_PARAMETER,
        30
      );
      await expectAnchorError(
        () =>
          program.methods
            .linkOffchainProposal(new Array(32).fill(0))
            .accountsPartial({ proposer: proposer.publicKey, proposal })
            .signers([proposer])
            .rpc({ commitment: "confirmed" }),
        "EmptyOffchainIdHash"
      );
    });

    it("refuses to link a proposal whose vote has already closed", async () => {
      // A link bolted onto a decided proposal would let a proposer point
      // a finished tally at an off-chain record after seeing how it went.
      const id = 4105;
      const { proposer, proposal } = await createFundedProposal(
        id,
        CATEGORY_PARAMETER,
        3
      );
      const account = await program.account.proposal.fetch(proposal);
      await sleepUntilChainTime(account.votingEndsAt.toNumber() + 1);
      await withBlockhashRetry(() =>
        program.methods
          .tallyAndFinalize()
          .accountsPartial({ proposal })
          .rpc({ commitment: "confirmed" })
      );

      await expectAnchorError(
        () =>
          program.methods
            .linkOffchainProposal(OFIP_0001_HASH)
            .accountsPartial({ proposer: proposer.publicKey, proposal })
            .signers([proposer])
            .rpc({ commitment: "confirmed" }),
        "NotInVotingState"
      );
    });
  });

  describe("update_governance_config", () => {
    // The deployed config was initialized with a treasury *owner* wallet in
    // forfeit_destination. refund_or_forfeit_deposit loads that field as a
    // TokenAccount unconditionally, so the whole instruction — refunds
    // included, not just the forfeit branch — could never execute. These
    // tests pin the property that made the bad value storable in the first
    // place: it is now supplied as an account, not a Pubkey. See OFS-4200 §7.

    function baseParams() {
      return {
        totalOpenSupply: new BN(TOTAL_OPEN_SUPPLY),
        quorumBps: QUORUM_BPS,
        thresholdSimpleBps: THRESHOLD_SIMPLE_BPS,
        thresholdTreasuryBps: THRESHOLD_TREASURY_BPS,
        thresholdUpgradeBps: THRESHOLD_UPGRADE_BPS,
        quorumUpgradeBps: QUORUM_UPGRADE_BPS,
        depositAmount: DEPOSIT_AMOUNT,
        voteLockSecs: new BN(604800),
      };
    }

    // `governanceConfig` is a shared singleton, and the successful cases
    // below write `baseParams()`'s week-long `voteLockSecs` into it for
    // real. Every later suite that has to carry a proposal past its
    // timelock — `presale.ts`'s ban gate, via `banWallet` — then waits a
    // week of chain time and hangs until mocha's timeout.
    //
    // Invisible until `list_wallet` started reading `vote_lock_secs` at
    // all: while it was admin-gated it never consulted the field, so
    // leaking a long lock cost nothing. `ban-list.ts` already restores
    // its own change to this field for the same reason.
    after(async () => {
      const current = await program.account.governanceConfig.fetch(
        governanceConfig
      );
      if (current.voteLockSecs.eq(SHARED_VOTE_LOCK_SECS)) return;
      await withBlockhashRetry(() =>
        program.methods
          .updateGovernanceConfig({
            ...baseParams(),
            voteLockSecs: SHARED_VOTE_LOCK_SECS,
          })
          .accountsPartial({
            admin: admin.publicKey,
            governanceConfig,
            mint,
            forfeitDestination: current.forfeitDestination,
          })
          .rpc({ commitment: "confirmed" })
      );
    });

    it("repoints forfeit_destination, and leaves every other field untouched", async () => {
      const before = await program.account.governanceConfig.fetch(
        governanceConfig
      );
      const replacement = await ata(mint, Keypair.generate().publicKey);

      const updateSignature = await withBlockhashRetry(() =>
        program.methods
          .updateGovernanceConfig(baseParams())
          .accountsPartial({
            admin: admin.publicKey,
            governanceConfig,
            mint,
            forfeitDestination: replacement,
          })
          .rpc({ commitment: "confirmed" })
      );

      // OFS-4100 §9.4. `admin` alone can move `forfeit_destination` and
      // `vote_lock_secs`, so the write has to leave a trace: one decides
      // where forfeited deposits go, the other how long an accepted
      // proposal is held before it can act.
      const event = (await eventsOf(updateSignature)).find(
        (e: any) => e.name === "governanceConfigUpdated"
      );
      expect(event, "governanceConfigUpdated event").to.not.be.undefined;
      expect(event.data.admin.toBase58()).to.equal(admin.publicKey.toBase58());
      expect(event.data.forfeitDestination.toBase58()).to.equal(
        replacement.toBase58()
      );
      expect(event.data.voteLockSecs.toString()).to.equal(
        baseParams().voteLockSecs.toString()
      );
      expect(event.data.quorumBps).to.equal(QUORUM_BPS);
      expect(event.data.depositAmount.toString()).to.equal(
        DEPOSIT_AMOUNT.toString()
      );

      const after = await program.account.governanceConfig.fetch(
        governanceConfig
      );
      expect(after.forfeitDestination.toBase58()).to.equal(
        replacement.toBase58()
      );
      // Everything not named in the params, and the params themselves,
      // must survive a round-trip unchanged — a config update that
      // quietly moves an unrelated field is the failure mode this guards.
      expect(after.admin.toBase58()).to.equal(before.admin.toBase58());
      expect(after.mint.toBase58()).to.equal(before.mint.toBase58());
      expect(after.depositVaultBump).to.equal(before.depositVaultBump);
      expect(after.bump).to.equal(before.bump);
      expect(after.totalOpenSupply.toString()).to.equal(
        before.totalOpenSupply.toString()
      );
      expect(after.quorumBps).to.equal(before.quorumBps);
      expect(after.depositAmount.toString()).to.equal(
        before.depositAmount.toString()
      );

      // Restore, so this block leaves the shared fixture as it found it.
      await withBlockhashRetry(() =>
        program.methods
          .updateGovernanceConfig(baseParams())
          .accountsPartial({
            admin: admin.publicKey,
            governanceConfig,
            mint,
            forfeitDestination,
          })
          .rpc({ commitment: "confirmed" })
      );
      const restored = await program.account.governanceConfig.fetch(
        governanceConfig
      );
      expect(restored.forfeitDestination.toBase58()).to.equal(
        forfeitDestination.toBase58()
      );
    });

    it("refuses a plain wallet where a token account is required", async () => {
      // The regression test for the actual defect: a system-owned address
      // cannot be stored, so the config cannot be put back into the state
      // that made refund_or_forfeit_deposit unexecutable.
      const wallet = Keypair.generate().publicKey;
      let failed = false;
      try {
        await program.methods
          .updateGovernanceConfig(baseParams())
          .accountsPartial({
            admin: admin.publicKey,
            governanceConfig,
            mint,
            forfeitDestination: wallet,
          })
          .rpc({ commitment: "confirmed" });
      } catch (err: any) {
        failed = true;
        // Anchor rejects at account load, before the handler runs, so this
        // is a constraint/ownership failure rather than a custom code.
        expect(String(err)).to.match(
          /AccountNotInitialized|owned by a different program|AccountOwnedByWrongProgram/
        );
      }
      expect(
        failed,
        "storing a wallet as forfeit_destination must fail"
      ).to.equal(true);
    });

    it("refuses a token account of the wrong mint", async () => {
      const otherMint = await createMint(
        connection,
        admin,
        admin.publicKey,
        null,
        MINT_DECIMALS,
        Keypair.generate(),
        { commitment: "confirmed" },
        TOKEN_2022_PROGRAM_ID
      );
      const wrongMintAta = await ata(otherMint, Keypair.generate().publicKey);
      await expectAnchorError(
        () =>
          program.methods
            .updateGovernanceConfig(baseParams())
            .accountsPartial({
              admin: admin.publicKey,
              governanceConfig,
              mint,
              forfeitDestination: wrongMintAta,
            })
            .rpc({ commitment: "confirmed" }),
        "ConstraintTokenMint"
      );
    });

    it("refuses a non-admin signer", async () => {
      const impostor = Keypair.generate();
      await airdrop(impostor.publicKey);
      await expectAnchorError(
        () =>
          program.methods
            .updateGovernanceConfig(baseParams())
            .accountsPartial({
              admin: impostor.publicKey,
              governanceConfig,
              mint,
              forfeitDestination,
            })
            .signers([impostor])
            .rpc({ commitment: "confirmed" }),
        "Unauthorized"
      );
    });

    it("rejects a vote lock longer than MAX_VOTE_LOCK_SECS", async () => {
      // `vote_lock_secs` is the delay before an accepted proposal may
      // act, and admin still writes it. Unbounded, one key could park it
      // past any horizon and leave every accepted proposal — including
      // one delisting a wallet — permanently unexecutable, which is the
      // same single-key veto over deposit access the ban list was
      // re-gated to remove, wearing a different hat.
      await expectAnchorError(
        () =>
          program.methods
            .updateGovernanceConfig({
              ...baseParams(),
              voteLockSecs: new BN(30 * 24 * 60 * 60 + 1),
            })
            .accountsPartial({
              admin: admin.publicKey,
              governanceConfig,
              mint,
              forfeitDestination,
            })
            .rpc({ commitment: "confirmed" }),
        "VoteLockTooLong"
      );
    });

    it("rejects out-of-range bps", async () => {
      await expectAnchorError(
        () =>
          program.methods
            .updateGovernanceConfig({ ...baseParams(), quorumBps: 10_001 })
            .accountsPartial({
              admin: admin.publicKey,
              governanceConfig,
              mint,
              forfeitDestination,
            })
            .rpc({ commitment: "confirmed" }),
        "InvalidBps"
      );
    });
  });
});
