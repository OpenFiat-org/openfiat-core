// OFS-7100 §12 — the protocol-enforced ban list.
//
// The claim under test is deliberately a cross-program one: listing a
// wallet once, in `governance`, must close deposit access in `escrow`
// and `staking` too, without those programs being separately notified.
// That is why these tests live in their own file rather than being
// split across `escrow.ts`/`staking.ts`/`governance.ts` — a per-program
// test would pass just as happily against three independent ban lists
// that had drifted apart, which is exactly the failure §12 exists to
// prevent. (`presale.ts` carries its own gate test, next to the sale
// fixture it needs.)
import * as anchor from "@anchor-lang/core";
import { Program, BN } from "@anchor-lang/core";
import { Escrow } from "../target/types/escrow";
import { Staking } from "../target/types/staking";
import { Governance } from "../target/types/governance";
import * as crypto from "crypto";
import {
  TOKEN_2022_PROGRAM_ID,
  mintTo,
  getOrCreateAssociatedTokenAccount,
} from "@solana/spl-token";
import {
  Keypair,
  PublicKey,
  SystemProgram,
  SYSVAR_RENT_PUBKEY,
} from "@solana/web3.js";
import { expect } from "chai";
import {
  getSharedMint,
  getSharedStakingConfig,
  getSharedGovernanceConfig,
  unit,
} from "./shared-fixtures";
import {
  ACTION_NONE,
  CATEGORY_STANDARDS,
  banWallet,
  castVote,
  createProposal,
  delistAction,
  delistingBuilder,
  finalize,
  getQuorumVoter,
  listAction,
  listingBuilder,
  nextProposalId,
  passProposal,
  proposalActionPda,
  proposalPda,
  sleepUntilChainTime,
  unbanWallet,
} from "./governance-cycle";

describe("ban list (OFS-7100 §12)", () => {
  anchor.setProvider(anchor.AnchorProvider.env());
  const provider = anchor.AnchorProvider.env();
  const connection = provider.connection;

  const escrow = anchor.workspace.escrow as Program<Escrow>;
  const staking = anchor.workspace.staking as Program<Staking>;
  const governance = anchor.workspace.governance as Program<Governance>;
  const admin = (provider.wallet as anchor.Wallet).payer;

  const ROLE_NODE_OPERATOR = { nodeOperator: {} };
  const REASON_SANCTIONS = { sanctions: {} };
  const REASON_STOLEN = { stolenFunds: {} };
  const REASON_SCAM = { scam: {} };
  const CATEGORY_PARAMETER = { parameter: {} };

  let mint: PublicKey;
  let stakingConfig: PublicKey;
  let stakeVault: PublicKey;
  let rewardsVault: PublicKey;
  let governanceConfig: PublicKey;
  let depositVault: PublicKey;
  let depositAmount: BN;
  let quorumVoter: Keypair;

  async function airdrop(pubkey: PublicKey, sol = 10) {
    const sig = await connection.requestAirdrop(pubkey, sol * 1_000_000_000);
    const latest = await connection.getLatestBlockhash();
    await connection.confirmTransaction({ signature: sig, ...latest });
  }

  async function ata(owner: PublicKey) {
    const acc = await getOrCreateAssociatedTokenAccount(
      connection,
      admin,
      mint,
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

  async function expectAnchorError(fn: () => Promise<unknown>, code: string) {
    try {
      await withBlockhashRetry(fn);
      expect.fail(`expected instruction to fail with ${code}, but it succeeded`);
    } catch (err: any) {
      const actual = err?.error?.errorCode?.code ?? String(err);
      expect(actual).to.equal(code);
    }
  }

  /// The canonical ban address for a wallet. Derived here from the same
  /// two inputs the on-chain constraint uses — the literal seed and
  /// `governance.programId` — so a test that passes proves the client
  /// and the program agree on where a ban lives.
  function banPda(wallet: PublicKey) {
    return PublicKey.findProgramAddressSync(
      [Buffer.from("ban"), wallet.toBuffer()],
      governance.programId
    )[0];
  }

  function stakeAccountPda(owner: PublicKey, roleByte: number) {
    return PublicKey.findProgramAddressSync(
      [Buffer.from("stake"), owner.toBuffer(), Buffer.from([roleByte])],
      staking.programId
    )[0];
  }

  function liquidityVaultPda(merchant: PublicKey) {
    return PublicKey.findProgramAddressSync(
      [Buffer.from("liquidity_vault"), merchant.toBuffer(), mint.toBuffer()],
      escrow.programId
    )[0];
  }

  function liquidityTokenVaultPda(merchant: PublicKey) {
    return PublicKey.findProgramAddressSync(
      [
        Buffer.from("liquidity_vault_tokens"),
        merchant.toBuffer(),
        mint.toBuffer(),
      ],
      escrow.programId
    )[0];
  }

  async function setUpMerchant(): Promise<{
    merchant: Keypair;
    from: PublicKey;
  }> {
    const merchant = Keypair.generate();
    await airdrop(merchant.publicKey);
    await withBlockhashRetry(() =>
      escrow.methods
        .createLiquidityVault()
        .accountsPartial({
          merchant: merchant.publicKey,
          mint,
          liquidityVault: liquidityVaultPda(merchant.publicKey),
          tokenVault: liquidityTokenVaultPda(merchant.publicKey),
          tokenProgram: TOKEN_2022_PROGRAM_ID,
          systemProgram: SystemProgram.programId,
          rent: SYSVAR_RENT_PUBKEY,
        })
        .signers([merchant])
        .rpc({ commitment: "confirmed" })
    );
    const from = await ata(merchant.publicKey);
    await mintTokens(from, unit(5000));
    return { merchant, from };
  }

  function depositLiquidity(merchant: Keypair, from: PublicKey, amount: BN) {
    return escrow.methods
      .depositLiquidity(amount)
      .accountsPartial({
        merchant: merchant.publicKey,
        liquidityVault: liquidityVaultPda(merchant.publicKey),
        tokenVault: liquidityTokenVaultPda(merchant.publicKey),
        from,
        mint,
        tokenProgram: TOKEN_2022_PROGRAM_ID,
      })
      .signers([merchant]);
  }

  async function setUpStaker(): Promise<{ owner: Keypair; from: PublicKey }> {
    const owner = Keypair.generate();
    await airdrop(owner.publicKey);
    await withBlockhashRetry(() =>
      staking.methods
        .initializeStakeAccount(ROLE_NODE_OPERATOR)
        .accountsPartial({
          owner: owner.publicKey,
          stakeAccount: stakeAccountPda(owner.publicKey, 2),
          systemProgram: SystemProgram.programId,
        })
        .signers([owner])
        .rpc({ commitment: "confirmed" })
    );
    const from = await ata(owner.publicKey);
    await mintTokens(from, unit(10000));
    return { owner, from };
  }

  function stake(owner: Keypair, from: PublicKey, amount: BN) {
    return staking.methods
      .stake(amount)
      .accountsPartial({
        owner: owner.publicKey,
        stakingConfig,
        stakeAccount: stakeAccountPda(owner.publicKey, 2),
        stakeVault,
        from,
        mint,
        tokenProgram: TOKEN_2022_PROGRAM_ID,
      })
      .signers([owner]);
  }

  /// Bans a wallet the only way it can now be banned — pass a proposal
  /// naming it, then execute that proposal — and returns the proposal
  /// that did it. See `governance-cycle.ts` for the machinery.
  const listWallet = (
    wallet: PublicKey,
    reason: any = REASON_SANCTIONS,
    evidenceHash?: number[]
  ) =>
    evidenceHash === undefined
      ? banWallet(governance, staking, wallet, reason)
      : banWallet(governance, staking, wallet, reason, evidenceHash);

  const delistWallet = (wallet: PublicKey) =>
    unbanWallet(governance, staking, wallet);

  const executeListing = (
    proposal: PublicKey,
    wallet: PublicKey,
    submitter?: Keypair
  ) => listingBuilder(governance, proposal, wallet, submitter);

  const executeDelisting = (
    proposal: PublicKey,
    wallet: PublicKey,
    submitter?: Keypair
  ) => delistingBuilder(governance, proposal, wallet, submitter);

  before(async () => {
    mint = await getSharedMint();
    ({ stakingConfig, stakeVault, rewardsVault } = await getSharedStakingConfig(
      staking
    ));
    ({ governanceConfig, depositVault, depositAmount } =
      await getSharedGovernanceConfig(governance));
    quorumVoter = await getQuorumVoter(staking, governance);
  });

  it("records a listing on-chain, readable with its reason and evidence", async () => {
    // §12.2: "The list MUST be readable on-chain". Not merely that the
    // gate works — that a third party can inspect *why*.
    const wallet = Keypair.generate().publicKey;
    const evidence = [...crypto.randomBytes(32)];
    const proposal = await listWallet(wallet, REASON_STOLEN, evidence);

    const record = await governance.account.banRecord.fetch(banPda(wallet));
    expect(record.wallet.toBase58()).to.equal(wallet.toBase58());
    expect(record.reason).to.deep.equal(REASON_STOLEN);
    expect([...record.evidenceHash]).to.deep.equal(evidence);
    expect(record.listedAt.toNumber()).to.be.greaterThan(0);
    // The record leads back to the vote, not to a signer. Under §15 that
    // is what an erroneously-listed wallet contests: the decision, the
    // tally behind it, and the evidence it rested on.
    expect(record.authorizingProposal.toBase58()).to.equal(
      proposal.toBase58()
    );
  });

  it("emits WalletListed on listing and WalletDelisted on delisting", async () => {
    // §12.2 requires both, so that an exclusion can be audited and a
    // reversal can be proven to have happened.
    const wallet = Keypair.generate().publicKey;
    const evidence = [...crypto.randomBytes(32)];

    const listingProposal = await passProposal(
      governance,
      staking,
      listAction(wallet, REASON_SANCTIONS, evidence)
    );
    const listed = await executeListing(listingProposal, wallet).simulate();
    const listedEvent = listed.events.find((e: any) => e.name === "walletListed");
    expect(listedEvent, "walletListed event").to.not.be.undefined;
    expect(listedEvent.data.wallet.toBase58()).to.equal(wallet.toBase58());
    expect(listedEvent.data.authorizingProposal.toBase58()).to.equal(
      listingProposal.toBase58()
    );
    await withBlockhashRetry(() =>
      executeListing(listingProposal, wallet).rpc({ commitment: "confirmed" })
    );

    const delistingProposal = await passProposal(governance, staking, delistAction(wallet));
    const delisted = await executeDelisting(delistingProposal, wallet).simulate();
    const delistedEvent = delisted.events.find(
      (e: any) => e.name === "walletDelisted"
    );
    expect(delistedEvent, "walletDelisted event").to.not.be.undefined;
    expect(delistedEvent.data.wallet.toBase58()).to.equal(wallet.toBase58());
    expect(delistedEvent.data.listedAt.toNumber()).to.be.greaterThan(0);
    // Both decisions are on the record, and they are different ones —
    // the audit trail §12.2 asks for is the pair, not either alone.
    expect(delistedEvent.data.authorizingProposal.toBase58()).to.equal(
      delistingProposal.toBase58()
    );
    expect(delistedEvent.data.listedByProposal.toBase58()).to.equal(
      listingProposal.toBase58()
    );
  });

  it("refuses a banned wallet's escrow liquidity deposit", async () => {
    const { merchant, from } = await setUpMerchant();
    await listWallet(merchant.publicKey);
    await expectAnchorError(
      () => depositLiquidity(merchant, from, unit(100)).rpc({ commitment: "confirmed" }),
      "WalletBanned"
    );
  });

  it("refuses a banned wallet's stake", async () => {
    const { owner, from } = await setUpStaker();
    await listWallet(owner.publicKey);
    await expectAnchorError(
      () => stake(owner, from, unit(1000)).rpc({ commitment: "confirmed" }),
      "WalletBanned"
    );
  });

  it("refuses a banned wallet's rewards-vault funding", async () => {
    // Gated despite being a pure one-way donation — see
    // `fund_rewards_vault`'s own doc comment for the reasoning.
    const funder = Keypair.generate();
    await airdrop(funder.publicKey);
    const from = await ata(funder.publicKey);
    await mintTokens(from, unit(1000));
    await listWallet(funder.publicKey);

    await expectAnchorError(
      () =>
        staking.methods
          .fundRewardsVault(unit(100))
          .accountsPartial({
            funder: funder.publicKey,
            mint,
            stakingConfig,
            rewardsVault,
            from,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
          })
          .signers([funder])
          .rpc({ commitment: "confirmed" }),
      "WalletBanned"
    );
  });

  it("refuses a banned wallet's proposal deposit", async () => {
    const proposer = Keypair.generate();
    await airdrop(proposer.publicKey);
    const from = await ata(proposer.publicKey);
    await mintTokens(from, depositAmount);
    await listWallet(proposer.publicKey);

    const id = 9001;
    const proposal = proposalPda(governance, id);

    await expectAnchorError(
      () =>
        governance.methods
          .createProposal(
            new BN(id),
            CATEGORY_PARAMETER,
            [...crypto.randomBytes(32)],
            [...crypto.randomBytes(32)],
            new BN(3),
            ACTION_NONE
          )
          .accountsPartial({
            proposer: proposer.publicKey,
            mint,
            governanceConfig,
            depositVault,
            from,
            proposal,
            proposalAction: proposalActionPda(governance, proposal),
            tokenProgram: TOKEN_2022_PROGRAM_ID,
            systemProgram: SystemProgram.programId,
            rent: SYSVAR_RENT_PUBKEY,
          })
          .signers([proposer])
          .rpc({ commitment: "confirmed" }),
      "WalletBanned"
    );
  });

  it("lets an unbanned wallet deposit, and restores access on delisting", async () => {
    // The control and the reversal in one narrative, because a ban list
    // that rejected everyone would pass every test above.
    const { owner, from } = await setUpStaker();
    const stakeAccount = stakeAccountPda(owner.publicKey, 2);

    await withBlockhashRetry(() =>
      stake(owner, from, unit(1000)).rpc({ commitment: "confirmed" })
    );
    expect(
      (await staking.account.stakeAccount.fetch(stakeAccount)).amount.toString()
    ).to.equal(unit(1000).toString());

    await listWallet(owner.publicKey);
    await expectAnchorError(
      () => stake(owner, from, unit(1000)).rpc({ commitment: "confirmed" }),
      "WalletBanned"
    );

    await delistWallet(owner.publicKey);
    await withBlockhashRetry(() =>
      stake(owner, from, unit(1000)).rpc({ commitment: "confirmed" })
    );
    expect(
      (await staking.account.stakeAccount.fetch(stakeAccount)).amount.toString()
    ).to.equal(unit(2000).toString());
  });

  describe("the substitution attack the gate exists to stop", () => {
    // Proof of non-existence is only as good as the address derivation.
    // If a banned caller could choose which account lands in the
    // `ban_record` slot, they would pass any empty account and the gate
    // would pass them — the ban would be decorative. These two tests are
    // the ones that would fail if `seeds`/`seeds::program` were dropped.

    it("rejects an unrelated empty account in the ban_record slot", async () => {
      const { owner, from } = await setUpStaker();
      await listWallet(owner.publicKey);

      // A fresh keypair: empty, system-owned, and therefore exactly what
      // `wallet_is_banned` would classify as "not banned" — if it were
      // ever allowed to reach the check.
      const decoy = Keypair.generate().publicKey;
      await expectAnchorError(
        () =>
          stake(owner, from, unit(1000))
            .accountsPartial({ banRecord: decoy })
            .rpc({ commitment: "confirmed" }),
        "ConstraintSeeds"
      );
    });

    it("rejects another wallet's ban address in the ban_record slot", async () => {
      // Subtler than the decoy above: this *is* a real ban PDA of the
      // real governance program, correctly derived and genuinely empty.
      // It is simply derived from someone else's key. Only the binding
      // to the signer's own key rejects it.
      const { owner, from } = await setUpStaker();
      await listWallet(owner.publicKey);

      const someoneElse = Keypair.generate().publicKey;
      const theirBanAddress = banPda(someoneElse);
      expect(
        await connection.getAccountInfo(theirBanAddress),
        "the borrowed address must be empty for this to be a real attack"
      ).to.be.null;

      await expectAnchorError(
        () =>
          stake(owner, from, unit(1000))
            .accountsPartial({ banRecord: theirBanAddress })
            .rpc({ commitment: "confirmed" }),
        "ConstraintSeeds"
      );
    });
  });

  describe("who may list (OFS-7100 §12.2 — only governance)", () => {
    // The whole point of this block: there is no key anywhere that can
    // list or delist a wallet. `GovernanceConfig.admin` is not read by
    // either instruction, and the accounts they do read are a vote.

    it("lets a stranger with no standing execute a passed proposal", async () => {
      // Authority comes from the vote, not from the sender. If a named
      // party had to submit, that party could refuse — and refusing to
      // execute a passed *delisting* is an unappealable ban by another
      // name.
      const wallet = Keypair.generate().publicKey;
      const stranger = Keypair.generate();
      await airdrop(stranger.publicKey);

      const proposal = await passProposal(
        governance,
        staking,
        listAction(wallet, REASON_SCAM, [...crypto.randomBytes(32)])
      );
      await withBlockhashRetry(() =>
        executeListing(proposal, wallet, stranger).rpc({
          commitment: "confirmed",
        })
      );
      expect(await connection.getAccountInfo(banPda(wallet))).to.not.be.null;
    });

    it("refuses to list without any proposal at all", async () => {
      // The former admin path, attempted by the former admin. Nothing
      // in `list_wallet` reads `GovernanceConfig.admin` any more, and
      // there is no account arrangement that stands in for a vote: the
      // proposal account is required, and it must be a real, accepted,
      // executable one.
      const wallet = Keypair.generate().publicKey;
      const neverVoted = await createProposal(
        governance,
        CATEGORY_STANDARDS,
        listAction(wallet, REASON_SCAM, [...crypto.randomBytes(32)]),
        3
      );
      await expectAnchorError(
        () =>
          executeListing(neverVoted, wallet).rpc({ commitment: "confirmed" }),
        "ProposalNotAccepted"
      );
    });

    it("refuses a proposal the vote rejected", async () => {
      const wallet = Keypair.generate().publicKey;
      const proposal = await createProposal(
        governance,
        CATEGORY_STANDARDS,
        listAction(wallet, REASON_SCAM, [...crypto.randomBytes(32)])
      );
      await castVote(governance, staking, proposal, quorumVoter, false);
      await finalize(governance, proposal);

      const account = await governance.account.proposal.fetch(proposal);
      expect(account.quorumMet, "quorum was met — it lost on the merits").to.equal(true);
      expect(account.state).to.deep.equal({ rejected: {} });

      await expectAnchorError(
        () => executeListing(proposal, wallet).rpc({ commitment: "confirmed" }),
        "ProposalNotAccepted"
      );
      expect(await connection.getAccountInfo(banPda(wallet))).to.be.null;
    });

    it("refuses a proposal that missed quorum", async () => {
      // Distinct from losing on the merits: this one had only for-votes
      // and still authorizes nothing, because too little of the supply
      // turned out to decide anything.
      const wallet = Keypair.generate().publicKey;
      const smallVoter = await setUpStaker();
      await withBlockhashRetry(() =>
        stake(smallVoter.owner, smallVoter.from, unit(1000)).rpc({
          commitment: "confirmed",
        })
      );

      const proposal = await createProposal(
        governance,
        CATEGORY_STANDARDS,
        listAction(wallet, REASON_SCAM, [...crypto.randomBytes(32)])
      );
      await castVote(governance, staking, proposal, smallVoter.owner, true);
      await finalize(governance, proposal);

      const account = await governance.account.proposal.fetch(proposal);
      expect(account.quorumMet).to.equal(false);
      expect(account.votesAgainst.toNumber()).to.equal(0);

      await expectAnchorError(
        () => executeListing(proposal, wallet).rpc({ commitment: "confirmed" }),
        "ProposalNotAccepted"
      );
      expect(await connection.getAccountInfo(banPda(wallet))).to.be.null;
    });

    it("refuses to execute inside the vote lock, and allows it once elapsed", async () => {
      // `vote_lock_secs` is the delay between a proposal passing and its
      // action taking effect — the window in which a wallet about to
      // lose all protocol access can see the decision coming. The
      // fixture's lock is one second, far too short to observe, so this
      // widens it for one proposal and narrows it again.
      const wallet = Keypair.generate().publicKey;
      const before = await governance.account.governanceConfig.fetch(
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
        voteLockSecs: new BN(45),
      };
      const setVoteLock = (voteLockSecs: BN) =>
        withBlockhashRetry(() =>
          governance.methods
            .updateGovernanceConfig({ ...params, voteLockSecs })
            .accountsPartial({
              admin: admin.publicKey,
              governanceConfig,
              mint,
              forfeitDestination: before.forfeitDestination,
            })
            .rpc({ commitment: "confirmed" })
        );

      await setVoteLock(new BN(45));
      const proposal = await createProposal(
        governance,
        CATEGORY_STANDARDS,
        listAction(wallet, REASON_STOLEN, [...crypto.randomBytes(32)])
      );
      await castVote(governance, staking, proposal, quorumVoter, true);
      const account = await governance.account.proposal.fetch(proposal);
      await sleepUntilChainTime(account.votingEndsAt.toNumber() + 1);
      await withBlockhashRetry(() =>
        governance.methods
          .tallyAndFinalize()
          .accountsPartial({ proposal })
          .rpc({ commitment: "confirmed" })
      );
      expect(
        (await governance.account.proposal.fetch(proposal)).state
      ).to.deep.equal({ accepted: {} });

      // Accepted, quorum met, un-executed — and still refused.
      await expectAnchorError(
        () => executeListing(proposal, wallet).rpc({ commitment: "confirmed" }),
        "ExecutionTimelockActive"
      );

      await setVoteLock(before.voteLockSecs);
      await withBlockhashRetry(() =>
        executeListing(proposal, wallet).rpc({ commitment: "confirmed" })
      );
      expect(
        await connection.getAccountInfo(banPda(wallet)),
        "the timelock was the only thing refusing it"
      ).to.not.be.null;
    });

    it("refuses to redeem a proposal naming wallet A against wallet B", async () => {
      // The binding that makes a vote mean what it said. Without it a
      // single passed listing would be a licence to ban anyone.
      const walletA = Keypair.generate().publicKey;
      const walletB = Keypair.generate().publicKey;
      const proposal = await passProposal(
        governance,
        staking,
        listAction(walletA, REASON_SCAM, [...crypto.randomBytes(32)])
      );

      await expectAnchorError(
        () => executeListing(proposal, walletB).rpc({ commitment: "confirmed" }),
        "ProposalActionMismatch"
      );
      expect(await connection.getAccountInfo(banPda(walletB))).to.be.null;

      // ...and the same proposal against the wallet it does name works,
      // so the rejection above was the binding and not a broken path.
      await withBlockhashRetry(() =>
        executeListing(proposal, walletA).rpc({ commitment: "confirmed" })
      );
      expect(await connection.getAccountInfo(banPda(walletA))).to.not.be.null;
    });

    it("refuses to list on a proposal that authorizes a delisting, or nothing", async () => {
      const wallet = Keypair.generate().publicKey;

      const delisting = await passProposal(governance, staking, delistAction(wallet));
      await expectAnchorError(
        () => executeListing(delisting, wallet).rpc({ commitment: "confirmed" }),
        "ProposalActionMismatch"
      );

      const inert = await passProposal(governance, staking, ACTION_NONE);
      await expectAnchorError(
        () => executeListing(inert, wallet).rpc({ commitment: "confirmed" }),
        "ProposalActionMismatch"
      );
      expect(await connection.getAccountInfo(banPda(wallet))).to.be.null;
    });

    it("refuses a ban action proposed under the wrong category", async () => {
      // The category fixes the quorum and majority a ban has to clear,
      // and fixes it identically for listing and delisting. Letting the
      // proposer pick it would let them pick their own bar.
      const wallet = Keypair.generate().publicKey;
      await expectAnchorError(
        () =>
          createProposal(
            governance,
            CATEGORY_PARAMETER,
            listAction(wallet, REASON_SCAM, [...crypto.randomBytes(32)])
          ),
        "WrongCategoryForBanAction"
      );
    });

    it("cannot execute the same proposal twice, even after a delisting", async () => {
      // The replay that would matter: list, readmit, then re-run the
      // original listing to undo the readmission. `executed` is set in
      // the same instruction that creates the record, so the second run
      // has nothing left to spend.
      const wallet = Keypair.generate().publicKey;
      const listing = await listWallet(wallet);
      await delistWallet(wallet);
      expect(await connection.getAccountInfo(banPda(wallet))).to.be.null;

      await expectAnchorError(
        () => executeListing(listing, wallet).rpc({ commitment: "confirmed" }),
        "AlreadyExecuted"
      );
      expect(
        await connection.getAccountInfo(banPda(wallet)),
        "the readmission stood"
      ).to.be.null;
    });

    it("refuses a second listing of an already-listed wallet", async () => {
      // `init`, not `init_if_needed`: silently overwriting would destroy
      // the original `listed_at` and evidence hash, which are what an
      // erroneously-listed wallet contests the listing with under §15.
      const wallet = Keypair.generate().publicKey;
      await listWallet(wallet);
      try {
        await listWallet(wallet);
        expect.fail("expected the second listing to fail");
      } catch (err: any) {
        expect(String(err)).to.match(/already in use/i);
      }
    });
  });
});
