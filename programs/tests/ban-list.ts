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

  async function listWallet(
    wallet: PublicKey,
    reason: any = REASON_SANCTIONS,
    evidenceHash: number[] = [...crypto.randomBytes(32)]
  ) {
    await withBlockhashRetry(() =>
      governance.methods
        .listWallet(wallet, reason, evidenceHash)
        .accountsPartial({
          admin: admin.publicKey,
          governanceConfig,
          banRecord: banPda(wallet),
          systemProgram: SystemProgram.programId,
        })
        .rpc({ commitment: "confirmed" })
    );
  }

  async function delistWallet(wallet: PublicKey) {
    await withBlockhashRetry(() =>
      governance.methods
        .delistWallet(wallet)
        .accountsPartial({
          admin: admin.publicKey,
          governanceConfig,
          banRecord: banPda(wallet),
        })
        .rpc({ commitment: "confirmed" })
    );
  }

  /// A funded merchant with an escrow liquidity vault ready to receive a
  /// deposit — the shortest path to exercising `deposit_liquidity`.
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

  before(async () => {
    mint = await getSharedMint();
    ({ stakingConfig, stakeVault, rewardsVault } = await getSharedStakingConfig(
      staking
    ));
    ({ governanceConfig, depositVault, depositAmount } =
      await getSharedGovernanceConfig(governance));
  });

  it("records a listing on-chain, readable with its reason and evidence", async () => {
    // §12.2: "The list MUST be readable on-chain". Not merely that the
    // gate works — that a third party can inspect *why*.
    const wallet = Keypair.generate().publicKey;
    const evidence = [...crypto.randomBytes(32)];
    await listWallet(wallet, REASON_STOLEN, evidence);

    const record = await governance.account.banRecord.fetch(banPda(wallet));
    expect(record.wallet.toBase58()).to.equal(wallet.toBase58());
    expect(record.reason).to.deep.equal(REASON_STOLEN);
    expect([...record.evidenceHash]).to.deep.equal(evidence);
    expect(record.listedBy.toBase58()).to.equal(admin.publicKey.toBase58());
    expect(record.listedAt.toNumber()).to.be.greaterThan(0);
  });

  it("emits WalletListed on listing and WalletDelisted on delisting", async () => {
    // §12.2 requires both, so that an exclusion can be audited and a
    // reversal can be proven to have happened.
    const wallet = Keypair.generate().publicKey;

    const listed = await governance.methods
      .listWallet(wallet, REASON_SANCTIONS, [...crypto.randomBytes(32)])
      .accountsPartial({
        admin: admin.publicKey,
        governanceConfig,
        banRecord: banPda(wallet),
        systemProgram: SystemProgram.programId,
      })
      .simulate();
    const listedEvent = listed.events.find((e: any) => e.name === "walletListed");
    expect(listedEvent, "walletListed event").to.not.be.undefined;
    expect(listedEvent.data.wallet.toBase58()).to.equal(wallet.toBase58());

    await listWallet(wallet);

    const delisted = await governance.methods
      .delistWallet(wallet)
      .accountsPartial({
        admin: admin.publicKey,
        governanceConfig,
        banRecord: banPda(wallet),
      })
      .simulate();
    const delistedEvent = delisted.events.find(
      (e: any) => e.name === "walletDelisted"
    );
    expect(delistedEvent, "walletDelisted event").to.not.be.undefined;
    expect(delistedEvent.data.wallet.toBase58()).to.equal(wallet.toBase58());
    expect(delistedEvent.data.listedAt.toNumber()).to.be.greaterThan(0);
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
    const proposal = PublicKey.findProgramAddressSync(
      [Buffer.from("proposal"), new BN(id).toArrayLike(Buffer, "le", 8)],
      governance.programId
    )[0];

    await expectAnchorError(
      () =>
        governance.methods
          .createProposal(
            new BN(id),
            CATEGORY_PARAMETER,
            [...crypto.randomBytes(32)],
            [...crypto.randomBytes(32)],
            new BN(3)
          )
          .accountsPartial({
            proposer: proposer.publicKey,
            mint,
            governanceConfig,
            depositVault,
            from,
            proposal,
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

  describe("who may list", () => {
    it("refuses a non-admin listing", async () => {
      const impostor = Keypair.generate();
      await airdrop(impostor.publicKey);
      const victim = Keypair.generate().publicKey;

      await expectAnchorError(
        () =>
          governance.methods
            .listWallet(victim, REASON_SCAM, [...crypto.randomBytes(32)])
            .accountsPartial({
              admin: impostor.publicKey,
              governanceConfig,
              banRecord: banPda(victim),
              systemProgram: SystemProgram.programId,
            })
            .signers([impostor])
            .rpc({ commitment: "confirmed" }),
        "Unauthorized"
      );
    });

    it("refuses a non-admin delisting", async () => {
      // The reversal path must be no *wider* than the exclusion path —
      // but it must also be no narrower, which the delisting test above
      // covers.
      const impostor = Keypair.generate();
      await airdrop(impostor.publicKey);
      const wallet = Keypair.generate().publicKey;
      await listWallet(wallet);

      await expectAnchorError(
        () =>
          governance.methods
            .delistWallet(wallet)
            .accountsPartial({
              admin: impostor.publicKey,
              governanceConfig,
              banRecord: banPda(wallet),
            })
            .signers([impostor])
            .rpc({ commitment: "confirmed" }),
        "Unauthorized"
      );
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
