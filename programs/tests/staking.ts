import * as anchor from "@anchor-lang/core";
import { Program, BN } from "@anchor-lang/core";
import { Staking } from "../target/types/staking";
import {
  TOKEN_2022_PROGRAM_ID,
  mintTo,
  getOrCreateAssociatedTokenAccount,
  getAccount,
  transferChecked,
} from "@solana/spl-token";
import { Keypair, PublicKey, SystemProgram } from "@solana/web3.js";
import { expect } from "chai";
import { getSharedOpenMint, getSharedStakingConfig, unit, MINT_DECIMALS } from "./shared-fixtures";

describe("staking", () => {
  anchor.setProvider(anchor.AnchorProvider.env());
  const provider = anchor.AnchorProvider.env();
  const connection = provider.connection;

  const program = anchor.workspace.staking as Program<Staking>;
  const admin = (provider.wallet as anchor.Wallet).payer;
  // Mirrors shared-fixtures' initialize call. The update tests write these
  // back unchanged: the point is proving the write path and its rejections,
  // not mutating a config the rest of the suite depends on — in particular
  // the 1-second unbonding period the withdraw tests need.
  const MIN_STAKE_BY_ROLE = [
    unit(1000),
    unit(10000),
    unit(1000),
    unit(5000),
    unit(1000),
    unit(1000),
    unit(1000),
  ];
  // One second for every role. The suite's withdraw tests need to be able
  // to actually wait one out, and OFS-4100 §4's real figures (24h / 3d /
  // 7d) are unreachable inside a test validator — the per-role *dispatch*
  // is what a test can prove, and `request_unstake` gets its own case
  // below that gives one role a distinguishable period.
  const UNBONDING_SECS_BY_ROLE = [
    new BN(1), new BN(1), new BN(1), new BN(1), new BN(1), new BN(1), new BN(1),
  ];

  const ROLE_NODE_OPERATOR = { nodeOperator: {} };
  const ROLE_ARBITRATOR = { arbitrator: {} };

  /// OPEN — the staked asset, and the only mint this program knows about
  /// (OFS-4100 §1, §4). Named `mint` rather than `openMint` because
  /// `StakingConfig` is scoped to exactly one and there is nothing here
  /// for it to be confused with.
  let mint: PublicKey;
  let stakingConfig: PublicKey;
  let stakeVault: PublicKey;
  let rewardsVault: PublicKey;
  let slashingAuthority: Keypair;
  let rewardsAuthority: Keypair;
  let slashDestination: PublicKey;

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

  // Takes a thunk rather than a promise so the send can be retried: a
  // validator-side "Blockhash not found" is a race in the send path, not
  // a verdict from the program, and asserting on it would fail a test
  // whose instruction never actually ran. `withBlockhashRetry` rethrows
  // everything else on the first attempt, so a genuine program error
  // still reaches the assertion below unchanged.
  async function expectAnchorError(send: () => Promise<unknown>, code: string) {
    try {
      await withBlockhashRetry(send);
      expect.fail(`expected instruction to fail with ${code}, but it succeeded`);
    } catch (err: any) {
      const actual = err?.error?.errorCode?.code ?? String(err);
      expect(actual).to.equal(code);
    }
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

  function stakeAccountPda(owner: PublicKey, roleByte: number) {
    return PublicKey.findProgramAddressSync(
      [Buffer.from("stake"), owner.toBuffer(), Buffer.from([roleByte])],
      program.programId,
    )[0];
  }

  before(async () => {
    mint = await getSharedOpenMint();
    ({ stakingConfig, stakeVault, rewardsVault, slashingAuthority, rewardsAuthority, slashDestination } =
      await getSharedStakingConfig(program));

    // Seed the rewards vault so claim_rewards has something real to pay out.
    const rewardsFunder = Keypair.generate();
    await airdrop(rewardsFunder.publicKey);
    const funderAta = await ata(mint, rewardsFunder.publicKey);
    await mintTokens(funderAta, unit(10000));
    await transferChecked(
      connection,
      rewardsFunder,
      funderAta,
      mint,
      rewardsVault,
      rewardsFunder,
      BigInt(unit(10000).toString()),
      MINT_DECIMALS,
      [],
      { commitment: "confirmed" },
      TOKEN_2022_PROGRAM_ID,
    );
  });

  describe("stake -> request_unstake -> withdraw_unstaked", () => {
    let owner: Keypair;
    let stakeAccount: PublicKey;
    // `stakeVault` is a single global vault shared with every other test
    // file/describe block in this run (a real production deployment
    // only ever has one too) — assertions below check the *delta* this
    // describe block's own actions cause, not the vault's absolute
    // total, which depends on how much other tests have already staked.
    let vaultBeforeStake: bigint;

    before(async () => {
      owner = Keypair.generate();
      await airdrop(owner.publicKey);
      stakeAccount = stakeAccountPda(owner.publicKey, 2); // NodeOperator = index 2

      await withBlockhashRetry(() =>
        program.methods
          .initializeStakeAccount(ROLE_NODE_OPERATOR)
          .accountsPartial({
            owner: owner.publicKey,
            stakeAccount,
            systemProgram: SystemProgram.programId,
          })
          .signers([owner])
          .rpc({ commitment: "confirmed" }),
      );

      const ownerAta = await ata(mint, owner.publicKey);
      await mintTokens(ownerAta, unit(20000));

      const before = await getAccount(connection, stakeVault, "confirmed", TOKEN_2022_PROGRAM_ID);
      vaultBeforeStake = before.amount;

      await withBlockhashRetry(() =>
        program.methods
          .stake(unit(15000))
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
    });

    it("records the staked amount and moves tokens into the shared stake vault", async () => {
      const account = await program.account.stakeAccount.fetch(stakeAccount);
      expect(account.amount.toString()).to.equal(unit(15000).toString());
      const vaultTokens = await getAccount(connection, stakeVault, "confirmed", TOKEN_2022_PROGRAM_ID);
      expect((vaultTokens.amount - vaultBeforeStake).toString()).to.equal(unit(15000).toString());
    });

    it("moves requested amount from `amount` to `unbonding_amount` immediately, with no token transfer yet", async () => {
      const vaultBeforeUnstake = (
        await getAccount(connection, stakeVault, "confirmed", TOKEN_2022_PROGRAM_ID)
      ).amount;

      await withBlockhashRetry(() =>
        program.methods
          .requestUnstake(unit(5000))
          .accountsPartial({ owner: owner.publicKey, stakingConfig, stakeAccount })
          .signers([owner])
          .rpc({ commitment: "confirmed" }),
      );

      const account = await program.account.stakeAccount.fetch(stakeAccount);
      expect(account.amount.toString()).to.equal(unit(10000).toString());
      expect(account.unbondingAmount.toString()).to.equal(unit(5000).toString());

      const vaultTokens = await getAccount(connection, stakeVault, "confirmed", TOKEN_2022_PROGRAM_ID);
      expect(vaultTokens.amount.toString()).to.equal(vaultBeforeUnstake.toString());
    });

    it("rejects withdraw_unstaked before the unbonding period has elapsed", async () => {
      const ownerAta = await ata(mint, owner.publicKey);
      await expectAnchorError(() =>
        program.methods
          .withdrawUnstaked()
          .accountsPartial({
            owner: owner.publicKey,
            mint,
            stakingConfig,
            stakeAccount,
            stakeVault,
            to: ownerAta,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
          })
          .signers([owner])
          .rpc({ commitment: "confirmed" }),
        "StillUnbonding",
      );
    });

    it("withdraws the unbonded amount once the unbonding period has elapsed", async () => {
      await new Promise((r) => setTimeout(r, 1500));
      const ownerAta = await ata(mint, owner.publicKey);

      await withBlockhashRetry(() =>
        program.methods
          .withdrawUnstaked()
          .accountsPartial({
            owner: owner.publicKey,
            mint,
            stakingConfig,
            stakeAccount,
            stakeVault,
            to: ownerAta,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
          })
          .signers([owner])
          .rpc({ commitment: "confirmed" }),
      );

      const account = await program.account.stakeAccount.fetch(stakeAccount);
      expect(account.unbondingAmount.toString()).to.equal("0");

      const ownerTokens = await getAccount(connection, ownerAta, "confirmed", TOKEN_2022_PROGRAM_ID);
      // Started with 20000, staked 15000 (5000 left), got 5000 back.
      expect(ownerTokens.amount.toString()).to.equal(unit(10000).toString());
    });
  });

  // The clock behind OFS-4100 §4's arbitrator stake age. `escrow`'s
  // `commit_dispute_vote` is the only consumer, but the invariants belong
  // here: whether a balance of zero can carry a running clock decides
  // whether an aged identity can exist with no capital behind it.
  describe("first_staked_at (the arbitrator stake-age clock)", () => {
    let owner: Keypair;
    let stakeAccount: PublicKey;
    let ownerAta: PublicKey;

    before(async () => {
      owner = Keypair.generate();
      await airdrop(owner.publicKey);
      stakeAccount = stakeAccountPda(owner.publicKey, 2); // NodeOperator
      await withBlockhashRetry(() =>
        program.methods
          .initializeStakeAccount(ROLE_NODE_OPERATOR)
          .accountsPartial({
            owner: owner.publicKey,
            stakeAccount,
            systemProgram: SystemProgram.programId,
          })
          .signers([owner])
          .rpc({ commitment: "confirmed" }),
      );
      ownerAta = await ata(mint, owner.publicKey);
      await mintTokens(ownerAta, unit(40000));
    });

    function stake(amount: BN) {
      return withBlockhashRetry(() =>
        program.methods
          .stake(amount)
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
    }

    it("starts at zero, because an account holding nothing has no age", async () => {
      // Not the current clock: an age that began before any tokens were
      // locked would let an attacker open accounts now and fund them thirty
      // days later at no cost, which is exactly what the requirement exists
      // to prevent.
      const account = await program.account.stakeAccount.fetch(stakeAccount);
      expect(account.amount.toString()).to.equal("0");
      expect(account.firstStakedAt.toString()).to.equal("0");
    });

    it("starts on the transition out of zero, and a top-up does not reset it", async () => {
      await stake(unit(10000));
      const afterFirst = await program.account.stakeAccount.fetch(stakeAccount);
      expect(afterFirst.firstStakedAt.gtn(0), "clock must start when tokens arrive").to.equal(true);

      await new Promise((r) => setTimeout(r, 2000));
      await stake(unit(5000));
      const afterTopUp = await program.account.stakeAccount.fetch(stakeAccount);
      // Resetting here would punish an honest arbitrator for adding stake,
      // and would buy nothing: `is_legal_balance` already forbids holding a
      // balance between zero and the role minimum, so no account can have
      // aged cheaply at a token balance and then jumped to a qualifying one.
      expect(afterTopUp.amount.toString()).to.equal(unit(15000).toString());
      expect(afterTopUp.firstStakedAt.toString()).to.equal(afterFirst.firstStakedAt.toString());
    });

    it("clears on a full exit, so age cannot outlive the capital behind it", async () => {
      await withBlockhashRetry(() =>
        program.methods
          .requestUnstake(unit(15000))
          .accountsPartial({ owner: owner.publicKey, stakingConfig, stakeAccount })
          .signers([owner])
          .rpc({ commitment: "confirmed" }),
      );
      const account = await program.account.stakeAccount.fetch(stakeAccount);
      expect(account.amount.toString()).to.equal("0");
      // Leaving it set would let an arbitrator withdraw entirely, wait, and
      // re-stake later while still presenting the age they accrued before
      // the tokens left — an aged identity with no capital behind it for the
      // gap.
      expect(account.firstStakedAt.toString()).to.equal("0");
    });

    it("refuses to migrate an account already in the current layout", async () => {
      // The migration is one-shot by construction: its length check only
      // passes against the 82-byte pre-migration layout. That is the whole
      // reason it can safely be permissionless — otherwise anyone could
      // call it repeatedly to reset somebody else's age clock.
      await expectAnchorError(
        () =>
          program.methods
            .migrateStakeAccount()
            .accountsPartial({
              payer: provider.wallet.publicKey,
              stakeAccount,
              systemProgram: SystemProgram.programId,
            })
            .rpc({ commitment: "confirmed" }),
        "StakeAccountAlreadyMigrated",
      );
    });
  });

  describe("slash", () => {
    it("moves slash_bps of the active stake to slash_destination, callable only by slashing_authority", async () => {
      const owner = Keypair.generate();
      await airdrop(owner.publicKey);
      const stakeAccount = stakeAccountPda(owner.publicKey, 1); // Arbitrator = index 1

      await withBlockhashRetry(() =>
        program.methods
          .initializeStakeAccount(ROLE_ARBITRATOR)
          .accountsPartial({ owner: owner.publicKey, stakeAccount, systemProgram: SystemProgram.programId })
          .signers([owner])
          .rpc({ commitment: "confirmed" }),
      );

      const ownerAta = await ata(mint, owner.publicKey);
      await mintTokens(ownerAta, unit(60000));
      await withBlockhashRetry(() =>
        program.methods
          .stake(unit(60000))
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

      await expectAnchorError(() =>
        program.methods
          .slash(1234)
          .accountsPartial({
            slashingAuthority: owner.publicKey, // wrong authority
            mint,
            stakingConfig,
            stakeAccount,
            stakeVault,
            destination: slashDestination,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
          })
          .signers([owner])
          .rpc({ commitment: "confirmed" }),
        "NotSlashingAuthority",
      );

      await withBlockhashRetry(() =>
        program.methods
          .slash(1234)
          .accountsPartial({
            slashingAuthority: slashingAuthority.publicKey,
            mint,
            stakingConfig,
            stakeAccount,
            stakeVault,
            destination: slashDestination,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
          })
          .signers([slashingAuthority])
          .rpc({ commitment: "confirmed" }),
      );

      const account = await program.account.stakeAccount.fetch(stakeAccount);
      // 10% of 60000 = 6000 slashed.
      expect(account.amount.toString()).to.equal(unit(54000).toString());
      expect(account.slashedTotal.toString()).to.equal(unit(6000).toString());

      const destTokens = await getAccount(connection, slashDestination, "confirmed", TOKEN_2022_PROGRAM_ID);
      expect(destTokens.amount.toString()).to.equal(unit(6000).toString());
    });
  });

  describe("distribute_reward + claim_rewards", () => {
    it("accrues then pays out a reward, callable only by rewards_authority", async () => {
      const owner = Keypair.generate();
      await airdrop(owner.publicKey);
      const stakeAccount = stakeAccountPda(owner.publicKey, 2);

      await withBlockhashRetry(() =>
        program.methods
          .initializeStakeAccount(ROLE_NODE_OPERATOR)
          .accountsPartial({ owner: owner.publicKey, stakeAccount, systemProgram: SystemProgram.programId })
          .signers([owner])
          .rpc({ commitment: "confirmed" }),
      );

      await expectAnchorError(() =>
        program.methods
          .distributeReward(new BN(1), unit(100))
          .accountsPartial({ rewardsAuthority: owner.publicKey, stakingConfig, stakeAccount })
          .signers([owner])
          .rpc({ commitment: "confirmed" }),
        "NotRewardsAuthority",
      );

      await withBlockhashRetry(() =>
        program.methods
          .distributeReward(new BN(1), unit(100))
          .accountsPartial({ rewardsAuthority: rewardsAuthority.publicKey, stakingConfig, stakeAccount })
          .signers([rewardsAuthority])
          .rpc({ commitment: "confirmed" }),
      );

      let account = await program.account.stakeAccount.fetch(stakeAccount);
      expect(account.pendingRewards.toString()).to.equal(unit(100).toString());

      const ownerAta = await ata(mint, owner.publicKey);
      await withBlockhashRetry(() =>
        program.methods
          .claimRewards()
          .accountsPartial({
            owner: owner.publicKey,
            mint,
            stakingConfig,
            stakeAccount,
            rewardsVault,
            to: ownerAta,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
          })
          .signers([owner])
          .rpc({ commitment: "confirmed" }),
      );

      account = await program.account.stakeAccount.fetch(stakeAccount);
      expect(account.pendingRewards.toString()).to.equal("0");
      const ownerTokens = await getAccount(connection, ownerAta, "confirmed", TOKEN_2022_PROGRAM_ID);
      expect(ownerTokens.amount.toString()).to.equal(unit(100).toString());
    });
  });

  describe("fund_rewards_vault", () => {
    it("tops the pool up from any wallet, and is what makes a claim possible at all", async () => {
      const before = await getAccount(connection, rewardsVault, "confirmed", TOKEN_2022_PROGRAM_ID);

      // Deliberately not the admin: funding is permissionless, because the
      // only thing it can do is increase a pool that pays stakers.
      const donor = Keypair.generate();
      await airdrop(donor.publicKey);
      const donorAta = await ata(mint, donor.publicKey);
      await mintTokens(donorAta, unit(500));

      await withBlockhashRetry(() =>
        program.methods
          .fundRewardsVault(unit(500))
          .accountsPartial({
            funder: donor.publicKey,
            mint,
            stakingConfig,
            rewardsVault,
            from: donorAta,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
          })
          .signers([donor])
          .rpc({ commitment: "confirmed" }),
      );

      const after = await getAccount(connection, rewardsVault, "confirmed", TOKEN_2022_PROGRAM_ID);
      expect((after.amount - before.amount).toString()).to.equal(unit(500).toString());
    });

    it("rejects a zero-amount deposit rather than emitting a meaningless event", async () => {
      const donor = Keypair.generate();
      await airdrop(donor.publicKey);
      const donorAta = await ata(mint, donor.publicKey);
      await expectAnchorError(() =>
        program.methods
          .fundRewardsVault(new BN(0))
          .accountsPartial({
            funder: donor.publicKey,
            mint,
            stakingConfig,
            rewardsVault,
            from: donorAta,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
          })
          .signers([donor])
          .rpc({ commitment: "confirmed" }),
        "ZeroAmount",
      );
    });
  });

  // `slash` is the one path that can leave a balance in the illegal middle —
  // non-zero but under the role minimum. `stake`/`request_unstake` refuse to
  // create one. The penalty stays proportional (a 10% slash is 10%, not a
  // wipeout), and the account simply stops carrying weight.
  describe("slash below the role minimum", () => {
    it("keeps the penalty proportional and zeroes the account's effective stake", async () => {
      const owner = Keypair.generate();
      await airdrop(owner.publicKey);
      const stakeAccount = stakeAccountPda(owner.publicKey, 1); // Arbitrator

      await withBlockhashRetry(() =>
        program.methods
          .initializeStakeAccount(ROLE_ARBITRATOR)
          .accountsPartial({ owner: owner.publicKey, stakeAccount, systemProgram: SystemProgram.programId })
          .signers([owner])
          .rpc({ commitment: "confirmed" }),
      );

      // Exactly the Arbitrator minimum — the common case, and the one where
      // sweeping the remainder to zero would have turned a 10% penalty into
      // total forfeiture.
      const ownerAta = await ata(mint, owner.publicKey);
      await mintTokens(ownerAta, unit(10000));
      await withBlockhashRetry(() =>
        program.methods
          .stake(unit(10000))
          .accountsPartial({
            owner: owner.publicKey,
            mint,
            stakingConfig,
            stakeAccount,
            stakeVault,
            from: ownerAta,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
          })
          .signers([owner])
          .rpc({ commitment: "confirmed" }),
      );

      await withBlockhashRetry(() =>
        program.methods
          .slash(7)
          .accountsPartial({
            slashingAuthority: slashingAuthority.publicKey,
            mint,
            stakingConfig,
            stakeAccount,
            stakeVault,
            destination: slashDestination,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
          })
          .signers([slashingAuthority])
          .rpc({ commitment: "confirmed" }),
      );

      // 10% taken, not 100%: the balance survives, below the 10,000 minimum.
      const account = await program.account.stakeAccount.fetch(stakeAccount);
      expect(account.amount.toString()).to.equal(unit(9000).toString());
      expect(account.slashedTotal.toString()).to.equal(unit(1000).toString());
      expect(account.amount.lt(unit(10000))).to.equal(true);
    });
  });

  // The minimums used to be stored and never read — a stake of one lamport
  // under any role was accepted. These prove they are enforced now, on the
  // way in and on the way out, for more than one role.
  describe("per-role minimum stake", () => {
    const ROLE_NOTIFICATION_PROVIDER = { notificationProvider: {} };

    async function freshStaker(roleArg: unknown, roleIndex: number, funding: BN) {
      const owner = Keypair.generate();
      await airdrop(owner.publicKey);
      const stakeAccount = stakeAccountPda(owner.publicKey, roleIndex);
      await withBlockhashRetry(() =>
        program.methods
          .initializeStakeAccount(roleArg as never)
          .accountsPartial({ owner: owner.publicKey, stakeAccount, systemProgram: SystemProgram.programId })
          .signers([owner])
          .rpc({ commitment: "confirmed" }),
      );
      const ownerAta = await ata(mint, owner.publicKey);
      await mintTokens(ownerAta, funding);
      return { owner, stakeAccount, ownerAta };
    }

    function stakeIx(ctx: { owner: Keypair; stakeAccount: PublicKey; ownerAta: PublicKey }, amount: BN) {
      return program.methods
        .stake(amount)
        .accountsPartial({
          owner: ctx.owner.publicKey,
          stakingConfig,
          stakeAccount: ctx.stakeAccount,
          stakeVault,
          from: ctx.ownerAta,
          mint,
          tokenProgram: TOKEN_2022_PROGRAM_ID,
        })
        .signers([ctx.owner]);
    }

    it("rejects a stake below the notification-provider minimum of 5,000 OPEN", async () => {
      const ctx = await freshStaker(ROLE_NOTIFICATION_PROVIDER, 3, unit(6000));
      await expectAnchorError(() =>
        stakeIx(ctx, unit(4999)).rpc({ commitment: "confirmed" }),
        "StakeBelowRoleMinimum",
      );
      // and nothing moved
      const account = await program.account.stakeAccount.fetch(ctx.stakeAccount);
      expect(account.amount.toString()).to.equal("0");
    });

    it("accepts a stake exactly at the notification-provider minimum", async () => {
      const ctx = await freshStaker(ROLE_NOTIFICATION_PROVIDER, 3, unit(6000));
      await withBlockhashRetry(() => stakeIx(ctx, unit(5000)).rpc({ commitment: "confirmed" }));
      const account = await program.account.stakeAccount.fetch(ctx.stakeAccount);
      expect(account.amount.toString()).to.equal(unit(5000).toString());
    });

    it("rejects a stake below the arbitrator minimum of 10,000 OPEN", async () => {
      const ctx = await freshStaker(ROLE_ARBITRATOR, 1, unit(12000));
      await expectAnchorError(() =>
        stakeIx(ctx, unit(9999)).rpc({ commitment: "confirmed" }),
        "StakeBelowRoleMinimum",
      );
    });

    it("lets a node operator clear its lower 1,000 minimum that would fail as an arbitrator", async () => {
      const ctx = await freshStaker(ROLE_NODE_OPERATOR, 2, unit(2000));
      await withBlockhashRetry(() => stakeIx(ctx, unit(1000)).rpc({ commitment: "confirmed" }));
      const account = await program.account.stakeAccount.fetch(ctx.stakeAccount);
      expect(account.amount.toString()).to.equal(unit(1000).toString());
    });

    it("refuses an unstake that would leave a balance below the minimum, but allows a full exit", async () => {
      const ctx = await freshStaker(ROLE_NOTIFICATION_PROVIDER, 3, unit(6000));
      await withBlockhashRetry(() => stakeIx(ctx, unit(5000)).rpc({ commitment: "confirmed" }));

      const unstake = (amount: BN) =>
        program.methods
          .requestUnstake(amount)
          .accountsPartial({ owner: ctx.owner.publicKey, stakingConfig, stakeAccount: ctx.stakeAccount })
          .signers([ctx.owner]);

      // 5000 -> 4999 would still read as staked while no longer qualifying.
      await expectAnchorError(
        () => unstake(unit(1)).rpc({ commitment: "confirmed" }),
        "StakeBelowRoleMinimum",
      );

      // Leaving entirely is always allowed - a minimum must not trap tokens.
      await withBlockhashRetry(() => unstake(unit(5000)).rpc({ commitment: "confirmed" }));
      const account = await program.account.stakeAccount.fetch(ctx.stakeAccount);
      expect(account.amount.toString()).to.equal("0");
      expect(account.unbondingAmount.toString()).to.equal(unit(5000).toString());
    });
  });

  describe("update_staking_config", () => {
    it("rejects a non-token-account as slash_destination — the defect that made slash unexecutable", async () => {
      // The deployed config stored a bare address with no account behind
      // it, so `slash` could never run: it requires that key to
      // deserialize as a token account. Taking the destination as an
      // account is what makes storing one impossible rather than merely
      // wrong — this is that exact address shape.
      const wallet = Keypair.generate().publicKey;
      await expectAnchorError(() =>
        program.methods
          .updateStakingConfig({
            minStakeByRole: MIN_STAKE_BY_ROLE,
            unbondingPeriodSecsByRole: UNBONDING_SECS_BY_ROLE,
            slashBps: 1000,
            slashingAuthority: slashingAuthority.publicKey,
            rewardsAuthority: rewardsAuthority.publicKey,
          })
          .accountsPartial({
            admin: admin.publicKey,
            stakingConfig,
            mint,
            slashDestination: wallet,
          })
          .signers([admin])
          .rpc({ commitment: "confirmed" }),
        "AccountNotInitialized",
      );
    });

    it("rejects a zero authority, which would be the same dead config in disguise", async () => {
      await expectAnchorError(() =>
        program.methods
          .updateStakingConfig({
            minStakeByRole: MIN_STAKE_BY_ROLE,
            unbondingPeriodSecsByRole: UNBONDING_SECS_BY_ROLE,
            slashBps: 1000,
            slashingAuthority: slashingAuthority.publicKey,
            rewardsAuthority: PublicKey.default,
          })
          .accountsPartial({
            admin: admin.publicKey,
            stakingConfig,
            mint,
            slashDestination,
          })
          .signers([admin])
          .rpc({ commitment: "confirmed" }),
        "ZeroAuthority",
      );
    });

    it("rejects a non-admin", async () => {
      const stranger = Keypair.generate();
      await airdrop(stranger.publicKey);
      await expectAnchorError(() =>
        program.methods
          .updateStakingConfig({
            minStakeByRole: MIN_STAKE_BY_ROLE,
            unbondingPeriodSecsByRole: UNBONDING_SECS_BY_ROLE,
            slashBps: 1000,
            slashingAuthority: slashingAuthority.publicKey,
            rewardsAuthority: rewardsAuthority.publicKey,
          })
          .accountsPartial({
            admin: stranger.publicKey,
            stakingConfig,
            mint,
            slashDestination,
          })
          .signers([stranger])
          .rpc({ commitment: "confirmed" }),
        "Unauthorized",
      );
    });

    it("rejects a zero unbonding period for a single role, not just for all of them", async () => {
      // A per-role array can be wrong for exactly one role, which a flat
      // field could not be. That role's stake would release in the same
      // slot it was requested, silently, while the other six looked fine.
      const oneRoleZeroed = [...UNBONDING_SECS_BY_ROLE];
      oneRoleZeroed[1] = new BN(0); // Arbitrator
      await expectAnchorError(() =>
        program.methods
          .updateStakingConfig({
            minStakeByRole: MIN_STAKE_BY_ROLE,
            unbondingPeriodSecsByRole: oneRoleZeroed,
            slashBps: 1000,
            slashingAuthority: slashingAuthority.publicKey,
            rewardsAuthority: rewardsAuthority.publicKey,
          })
          .accountsPartial({
            admin: admin.publicKey,
            stakingConfig,
            mint,
            slashDestination,
          })
          .signers([admin])
          .rpc({ commitment: "confirmed" }),
        "InvalidUnbondingPeriod",
      );
    });

    // The OFS-4100 §4 rework, exercised end to end: the 500-OPEN floors,
    // and an unbonding period that differs by role.
    it("applies the OFS-4100 §4 floors and gives each role its own unbonding period", async () => {
      const OPEN = (n: number) => unit(n);
      // Merchant and Arbitrator drop to a 500 floor; everything else keeps
      // the figures the deployment already had.
      const FLOORS = [OPEN(500), OPEN(500), OPEN(1000), OPEN(5000), OPEN(1000), OPEN(1000), OPEN(1000)];
      // Real §4 periods are 24h/3d/7d, none of which a test validator can
      // wait out. What is testable is that the *dispatch* is per role, so
      // the arbitrator gets a period an order of magnitude off everyone
      // else's and the two are compared against each other.
      const ARBITRATOR_SECS = 300;
      const PERIODS = [
        new BN(1), new BN(ARBITRATOR_SECS), new BN(1), new BN(1), new BN(1), new BN(1), new BN(1),
      ];

      await withBlockhashRetry(() =>
        program.methods
          .updateStakingConfig({
            minStakeByRole: FLOORS,
            unbondingPeriodSecsByRole: PERIODS,
            slashBps: 500, // 5%, per §4
            slashingAuthority: slashingAuthority.publicKey,
            rewardsAuthority: rewardsAuthority.publicKey,
          })
          .accountsPartial({ admin: admin.publicKey, stakingConfig, mint, slashDestination })
          .signers([admin])
          .rpc({ commitment: "confirmed" }),
      );

      const config = await program.account.stakingConfig.fetch(stakingConfig);
      expect(config.minStakeByRole.map(String)).to.deep.equal(FLOORS.map(String));
      expect(config.unbondingPeriodSecsByRole.map(String)).to.deep.equal(PERIODS.map(String));
      expect(config.slashBps).to.equal(500);

      async function stakeThenRequest(roleArg: unknown, roleIndex: number, stakeAmount: BN, unstakeAmount: BN) {
        const owner = Keypair.generate();
        await airdrop(owner.publicKey);
        const stakeAccount = stakeAccountPda(owner.publicKey, roleIndex);
        await withBlockhashRetry(() =>
          program.methods
            .initializeStakeAccount(roleArg as never)
            .accountsPartial({ owner: owner.publicKey, stakeAccount, systemProgram: SystemProgram.programId })
            .signers([owner])
            .rpc({ commitment: "confirmed" }),
        );
        const ownerAta = await ata(mint, owner.publicKey);
        await mintTokens(ownerAta, stakeAmount);
        await withBlockhashRetry(() =>
          program.methods
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
        await withBlockhashRetry(() =>
          program.methods
            .requestUnstake(unstakeAmount)
            .accountsPartial({ owner: owner.publicKey, stakingConfig, stakeAccount })
            .signers([owner])
            .rpc({ commitment: "confirmed" }),
        );
        return program.account.stakeAccount.fetch(stakeAccount);
      }

      // 10,000 down to 500 — legal only because the floor moved. It was
      // the old minimum, so this also covers the case the rework had to
      // not break: lowering a minimum can only widen the set of legal
      // balances, never strand a position that was already open.
      const arbitrator = await stakeThenRequest(ROLE_ARBITRATOR, 1, OPEN(10000), OPEN(9500));
      expect(arbitrator.amount.toString()).to.equal(OPEN(500).toString());

      const nodeOperator = await stakeThenRequest(ROLE_NODE_OPERATOR, 2, OPEN(1000), OPEN(1000));
      expect(nodeOperator.amount.toString()).to.equal("0");

      // Clock-independent, and independent of how long the setup above
      // took: the arbitrator's release is `t_a + 300`, the node
      // operator's is `t_n + 1` with `t_n > t_a`, so the gap is
      // `299 - (t_n - t_a)` — strictly below the arbitrator's period and,
      // unless the second setup took minutes, well above half of it. A
      // flat period would put the gap at zero.
      const gap =
        arbitrator.unbondingReleaseAt.toNumber() -
        nodeOperator.unbondingReleaseAt.toNumber();
      expect(gap).to.be.lessThan(ARBITRATOR_SECS);
      expect(gap).to.be.greaterThan(ARBITRATOR_SECS / 2);

      // Restore, so the rest of the suite keeps the 1-second period its
      // withdraw tests wait out. Not left to the test that follows: a
      // fixture this one moved is this one's to put back.
      await withBlockhashRetry(() =>
        program.methods
          .updateStakingConfig({
            minStakeByRole: MIN_STAKE_BY_ROLE,
            unbondingPeriodSecsByRole: UNBONDING_SECS_BY_ROLE,
            slashBps: 1000,
            slashingAuthority: slashingAuthority.publicKey,
            rewardsAuthority: rewardsAuthority.publicKey,
          })
          .accountsPartial({ admin: admin.publicKey, stakingConfig, mint, slashDestination })
          .signers([admin])
          .rpc({ commitment: "confirmed" }),
      );
    });

    it("writes a real token account through and leaves slash executable", async () => {
      await withBlockhashRetry(() =>
        program.methods
          .updateStakingConfig({
            minStakeByRole: MIN_STAKE_BY_ROLE,
            unbondingPeriodSecsByRole: UNBONDING_SECS_BY_ROLE,
            slashBps: 1000,
            slashingAuthority: slashingAuthority.publicKey,
            rewardsAuthority: rewardsAuthority.publicKey,
          })
          .accountsPartial({
            admin: admin.publicKey,
            stakingConfig,
            mint,
            slashDestination,
          })
          .signers([admin])
          .rpc({ commitment: "confirmed" }),
      );
      const config = await program.account.stakingConfig.fetch(stakingConfig);
      expect(config.slashDestination.toBase58()).to.equal(slashDestination.toBase58());
      expect(config.rewardsAuthority.toBase58()).to.equal(rewardsAuthority.publicKey.toBase58());
      expect(config.slashingAuthority.toBase58()).to.equal(slashingAuthority.publicKey.toBase58());
    });
  });

});
