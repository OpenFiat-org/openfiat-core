// Producing a *passed* governance proposal, for the specs that need one.
//
// Since OFS-7100 §12.2 was actually implemented, a ban is no longer an
// admin call — it is the execution of a proposal that was created, voted
// on, tallied and timed out of its vote lock. Three spec files need one
// (`ban-list.ts` for the ban list itself, `presale.ts` for its own gate,
// and anything that follows), and hand-rolling the cycle in each would
// mean three chances to get "accepted" subtly wrong and prove less than
// the file claims. It lives here once instead.
//
// The quorum voter is memoized across the whole run for the same reason
// the config singletons are: quorum is 10% of the total OPEN supply, and
// staking that once is fast while staking it per spec file is not.
// `VoteRecord` is keyed by (proposal, voter), so one staker can carry
// any number of separate proposals.
import * as anchor from "@anchor-lang/core";
import { Program, BN } from "@anchor-lang/core";
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
import {
  getSharedOpenMint,
  getSharedStakingConfig,
  getSharedGovernanceConfig,
} from "./shared-fixtures";

const provider = anchor.AnchorProvider.env();
anchor.setProvider(provider);
const connection = provider.connection;
const admin = (provider.wallet as anchor.Wallet).payer;

export const CATEGORY_STANDARDS = { standards: {} };
export const ACTION_NONE = { none: {} };
export const REASON_SANCTIONS = { sanctions: {} };

/** Mirrors governance::constants::MIN_VOTING_PERIOD_SECS (F-04's [DEVNET
 *  VALUE] floor) — the shortest window `create_proposal` now accepts.
 *  Long enough that creating the proposal and casting the quorum vote
 *  both land inside it on a local validator, short enough that a suite
 *  is not dominated by waiting. */
export const VOTING_PERIOD_SECS = 30;

const ROLE_NODE_OPERATOR = { nodeOperator: {} };
const ROLE_NODE_OPERATOR_BYTE = 2;

export const listAction = (
  wallet: PublicKey,
  reason: any,
  evidenceHash: number[],
) => ({ listWallet: { wallet, reason, evidenceHash } });

export const delistAction = (wallet: PublicKey) => ({ delistWallet: { wallet } });

let proposalIdCounter = 70_000;
export function nextProposalId(): number {
  return proposalIdCounter++;
}

/** Derived, not fetched, so the instruction builders below stay
 *  synchronous — the account still has to have been initialized by
 *  `getSharedGovernanceConfig` before any of them is sent. */
export function governanceConfigPda(governance: Program<Governance>) {
  return PublicKey.findProgramAddressSync(
    [Buffer.from("governance_config")],
    governance.programId,
  )[0];
}

export function proposalPda(governance: Program<Governance>, id: number) {
  return PublicKey.findProgramAddressSync(
    [Buffer.from("proposal"), new BN(id).toArrayLike(Buffer, "le", 8)],
    governance.programId,
  )[0];
}

export function proposalActionPda(
  governance: Program<Governance>,
  proposal: PublicKey,
) {
  return PublicKey.findProgramAddressSync(
    [Buffer.from("proposal_action"), proposal.toBuffer()],
    governance.programId,
  )[0];
}

export function voteRecordPda(
  governance: Program<Governance>,
  proposal: PublicKey,
  voter: PublicKey,
) {
  return PublicKey.findProgramAddressSync(
    [Buffer.from("vote"), proposal.toBuffer(), voter.toBuffer()],
    governance.programId,
  )[0];
}

export function banPda(governance: Program<Governance>, wallet: PublicKey) {
  return PublicKey.findProgramAddressSync(
    [Buffer.from("ban"), wallet.toBuffer()],
    governance.programId,
  )[0];
}

function stakeAccountPda(staking: Program<Staking>, owner: PublicKey) {
  return PublicKey.findProgramAddressSync(
    [Buffer.from("stake"), owner.toBuffer(), Buffer.from([ROLE_NODE_OPERATOR_BYTE])],
    staking.programId,
  )[0];
}

async function airdrop(pubkey: PublicKey, sol = 10) {
  const sig = await connection.requestAirdrop(pubkey, sol * 1_000_000_000);
  const latest = await connection.getLatestBlockhash();
  await connection.confirmTransaction({ signature: sig, ...latest });
}

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

async function mintTokens(mint: PublicKey, dest: PublicKey, amount: BN) {
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

export async function withBlockhashRetry<T>(
  fn: () => Promise<T>,
  attempts = 4,
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

/** The validator's own clock, not this process's. Every deadline in
 *  `governance` is compared against `Clock::unix_timestamp`, so sleeping
 *  against wall-clock time would measure the wrong thing. */
export async function chainTime(): Promise<number> {
  const slot = await connection.getSlot("confirmed");
  const time = await connection.getBlockTime(slot);
  if (time === null) throw new Error("validator returned no block time");
  return time;
}

export async function sleepUntilChainTime(target: number) {
  for (;;) {
    const now = await chainTime();
    if (now >= target) return;
    await new Promise((r) =>
      setTimeout(r, Math.min(1500, (target - now) * 1000 + 250)),
    );
  }
}

/** A staker holding 120% of the whole-supply quorum requirement, so one
 *  for-vote carries any proposal past both quorum and a simple majority. */
let quorumVoterPromise: Promise<Keypair> | null = null;
export function getQuorumVoter(
  staking: Program<Staking>,
  governance: Program<Governance>,
): Promise<Keypair> {
  if (!quorumVoterPromise) {
    quorumVoterPromise = (async () => {
      // OPEN: quorum is a fraction of the OPEN supply, so the stake that
      // meets it has to be denominated in the same token.
      const mint = await getSharedOpenMint();
      const { stakingConfig, stakeVault } = await getSharedStakingConfig(staking);
      const { totalOpenSupply, quorumBps } =
        await getSharedGovernanceConfig(governance);

      const owner = Keypair.generate();
      await airdrop(owner.publicKey);
      await withBlockhashRetry(() =>
        staking.methods
          .initializeStakeAccount(ROLE_NODE_OPERATOR)
          .accountsPartial({
            owner: owner.publicKey,
            stakeAccount: stakeAccountPda(staking, owner.publicKey),
            systemProgram: SystemProgram.programId,
          })
          .signers([owner])
          .rpc({ commitment: "confirmed" }),
      );

      const amount = totalOpenSupply
        .mul(new BN(quorumBps))
        .div(new BN(10_000))
        .mul(new BN(12))
        .div(new BN(10));
      const from = await ata(mint, owner.publicKey);
      await mintTokens(mint, from, amount);
      await withBlockhashRetry(() =>
        staking.methods
          .stake(amount)
          .accountsPartial({
            owner: owner.publicKey,
            stakingConfig,
            stakeAccount: stakeAccountPda(staking, owner.publicKey),
            stakeVault,
            from,
            mint,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
          })
          .signers([owner])
          .rpc({ commitment: "confirmed" }),
      );
      return owner;
    })();
  }
  return quorumVoterPromise;
}

/** Creates a proposal from a throwaway, freshly-funded proposer. */
export async function createProposal(
  governance: Program<Governance>,
  category: any,
  action: any,
  votingPeriodSecs = VOTING_PERIOD_SECS,
  id = nextProposalId(),
): Promise<PublicKey> {
  // OPEN — the proposal deposit is denominated in the governance token,
  // not in whatever a trade happens to settle in.
  const mint = await getSharedOpenMint();
  const { governanceConfig, depositVault, depositAmount } =
    await getSharedGovernanceConfig(governance);

  const proposer = Keypair.generate();
  await airdrop(proposer.publicKey);
  const from = await ata(mint, proposer.publicKey);
  await mintTokens(mint, from, depositAmount);

  const proposal = proposalPda(governance, id);
  await withBlockhashRetry(() =>
    governance.methods
      .createProposal(
        new BN(id),
        category,
        [...crypto.randomBytes(32)],
        [...crypto.randomBytes(32)],
        new BN(votingPeriodSecs),
        action,
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
  );
  return proposal;
}

export async function castVote(
  governance: Program<Governance>,
  staking: Program<Staking>,
  proposal: PublicKey,
  voter: Keypair,
  inFavor: boolean,
) {
  const { governanceConfig } = await getSharedGovernanceConfig(governance);
  await withBlockhashRetry(() =>
    governance.methods
      .castVote(inFavor, ROLE_NODE_OPERATOR)
      .accountsPartial({
        voter: voter.publicKey,
        governanceConfig,
        proposal,
        voterStake: stakeAccountPda(staking, voter.publicKey),
        voteRecord: voteRecordPda(governance, proposal, voter.publicKey),
        systemProgram: SystemProgram.programId,
      })
      .signers([voter])
      .rpc({ commitment: "confirmed" }),
  );
}

/** Closes voting, tallies, and waits out the execution timelock, so the
 *  proposal is executable *now* if the vote accepted it. */
export async function finalize(
  governance: Program<Governance>,
  proposal: PublicKey,
) {
  const { governanceConfig } = await getSharedGovernanceConfig(governance);
  const account = await governance.account.proposal.fetch(proposal);
  await sleepUntilChainTime(account.votingEndsAt.toNumber() + 1);
  await withBlockhashRetry(() =>
    governance.methods
      .tallyAndFinalize()
      .accountsPartial({ proposal })
      .rpc({ commitment: "confirmed" }),
  );
  const config = await governance.account.governanceConfig.fetch(governanceConfig);
  await sleepUntilChainTime(
    account.votingEndsAt.toNumber() + config.voteLockSecs.toNumber() + 1,
  );
}

/** A proposal carried by the quorum voter, tallied, and past its
 *  timelock — i.e. genuinely executable. */
export async function passProposal(
  governance: Program<Governance>,
  staking: Program<Staking>,
  action: any,
  category: any = CATEGORY_STANDARDS,
): Promise<PublicKey> {
  const voter = await getQuorumVoter(staking, governance);
  const proposal = await createProposal(governance, category, action);
  await castVote(governance, staking, proposal, voter, true);
  await finalize(governance, proposal);

  // A fixture that quietly produced a *rejected* proposal would make
  // every "cannot execute" test below pass for the wrong reason.
  const account = await governance.account.proposal.fetch(proposal);
  if (!account.quorumMet || account.state.accepted === undefined) {
    throw new Error(
      `passProposal produced a proposal that did not pass: ${JSON.stringify(
        account.state,
      )}, quorumMet=${account.quorumMet}`,
    );
  }
  return proposal;
}

/** `submitter` defaults to the provider wallet purely because it is
 *  already funded — it holds no authority in either builder below. */
export function listingBuilder(
  governance: Program<Governance>,
  proposal: PublicKey,
  wallet: PublicKey,
  submitter?: Keypair,
) {
  const builder = governance.methods.listWallet(wallet).accountsPartial({
    submitter: (submitter ?? admin).publicKey,
    governanceConfig: governanceConfigPda(governance),
    proposal,
    proposalAction: proposalActionPda(governance, proposal),
    banRecord: banPda(governance, wallet),
    systemProgram: SystemProgram.programId,
  });
  return submitter ? builder.signers([submitter]) : builder;
}

export function delistingBuilder(
  governance: Program<Governance>,
  proposal: PublicKey,
  wallet: PublicKey,
  submitter?: Keypair,
) {
  const builder = governance.methods.delistWallet(wallet).accountsPartial({
    submitter: (submitter ?? admin).publicKey,
    governanceConfig: governanceConfigPda(governance),
    proposal,
    proposalAction: proposalActionPda(governance, proposal),
    banRecord: banPda(governance, wallet),
  });
  return submitter ? builder.signers([submitter]) : builder;
}

/** Bans a wallet the only way it can now be banned: pass a proposal that
 *  names it, then execute that proposal. Returns the proposal. */
export async function banWallet(
  governance: Program<Governance>,
  staking: Program<Staking>,
  wallet: PublicKey,
  reason: any = REASON_SANCTIONS,
  evidenceHash: number[] = [...crypto.randomBytes(32)],
): Promise<PublicKey> {
  const proposal = await passProposal(
    governance,
    staking,
    listAction(wallet, reason, evidenceHash),
  );
  await withBlockhashRetry(() =>
    listingBuilder(governance, proposal, wallet).rpc({ commitment: "confirmed" }),
  );
  return proposal;
}

export async function unbanWallet(
  governance: Program<Governance>,
  staking: Program<Staking>,
  wallet: PublicKey,
): Promise<PublicKey> {
  const proposal = await passProposal(governance, staking, delistAction(wallet));
  await withBlockhashRetry(() =>
    delistingBuilder(governance, proposal, wallet).rpc({
      commitment: "confirmed",
    }),
  );
  return proposal;
}
