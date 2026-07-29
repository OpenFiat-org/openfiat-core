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
import { getSharedMint, getSharedStakingConfig, unit, MINT_DECIMALS } from "./shared-fixtures";

describe("staking", () => {
  anchor.setProvider(anchor.AnchorProvider.env());
  const provider = anchor.AnchorProvider.env();
  const connection = provider.connection;

  const program = anchor.workspace.staking as Program<Staking>;
  const admin = (provider.wallet as anchor.Wallet).payer;

  const ROLE_NODE_OPERATOR = { nodeOperator: {} };
  const ROLE_ARBITRATOR = { arbitrator: {} };

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

  async function expectAnchorError(p: Promise<unknown>, code: string) {
    try {
      await p;
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
    mint = await getSharedMint();
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
      await expectAnchorError(
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

      await expectAnchorError(
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

      await expectAnchorError(
        program.methods
          .distributeReward(unit(100))
          .accountsPartial({ rewardsAuthority: owner.publicKey, stakingConfig, stakeAccount })
          .signers([owner])
          .rpc({ commitment: "confirmed" }),
        "NotRewardsAuthority",
      );

      await withBlockhashRetry(() =>
        program.methods
          .distributeReward(unit(100))
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
      await expectAnchorError(
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
      await expectAnchorError(
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
      await expectAnchorError(unstake(unit(1)).rpc({ commitment: "confirmed" }), "StakeBelowRoleMinimum");

      // Leaving entirely is always allowed - a minimum must not trap tokens.
      await withBlockhashRetry(() => unstake(unit(5000)).rpc({ commitment: "confirmed" }));
      const account = await program.account.stakeAccount.fetch(ctx.stakeAccount);
      expect(account.amount.toString()).to.equal("0");
      expect(account.unbondingAmount.toString()).to.equal(unit(5000).toString());
    });
  });

});
