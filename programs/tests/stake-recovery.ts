/**
 * The stake-recovery relay, end to end across both programs
 * (OFS-4100 §9.3, OFS-4200 §1).
 *
 * # Why this suite used to have its own ledger, and no longer does
 *
 * `tests/shared-fixtures.ts` once denominated `StakingConfig` in the
 * *settlement* mint while the arbitration pool held OPEN. That
 * configuration cannot exist in production — OFS-4100 §1 and §4 make OPEN
 * the staked asset and §6 makes the dispute filing fee OPEN too, so a
 * merchant's stake and their arbitration deposit are the same token — and
 * it made this relay untestable: recovery moves tokens out of the stake
 * vault and into the merchant's OPEN liquidity vault, a transfer the SPL
 * token program rejects outright when those mints differ. Since the three
 * protocol singletons can each be initialized only once per ledger, the
 * fix was not available from inside a spec file, and this suite built its
 * own singletons on a ledger of its own.
 *
 * The fixture is now correct, so it does not have to. What this suite
 * needs beyond the fixture is one *mutable* parameter — a non-zero
 * `dispute_filing_fee`, where the shared default is zero — and
 * `update_fee_config` exists precisely to write that. It is perturbed in
 * `before` and handed back in `after`, the same contract
 * `settlement-mints.ts` keeps with the settlement allowlist.
 *
 * That the relay now runs against the fixtures every other suite runs
 * against is the point, not a convenience: it is the assertion that this
 * chain is the same chain, rather than a second one arranged to make the
 * relay work.
 *
 * # What it proves
 *
 * That the two programs agree about a debt neither of them can write on
 * the other's behalf. `escrow` records what a merchant failed to cover;
 * `staking` reads that account, collects against it, and records what it
 * took; `escrow` reads *that* and turns it back into vault liquidity that
 * completes the deposit. No CPI in either direction, no authority, and no
 * caller-supplied amount anywhere in the chain.
 */
import * as anchor from "@anchor-lang/core";
import { Program, BN } from "@anchor-lang/core";
import { Escrow } from "../target/types/escrow";
import { Staking } from "../target/types/staking";
import {
  TOKEN_2022_PROGRAM_ID,
  mintTo,
  getAccount,
  getOrCreateAssociatedTokenAccount,
} from "@solana/spl-token";
import {
  Keypair,
  PublicKey,
  SystemProgram,
  SYSVAR_RENT_PUBKEY,
  SYSVAR_SLOT_HASHES_PUBKEY,
} from "@solana/web3.js";
import { expect } from "chai";
import {
  SHARED_FEE_PARAMS,
  SharedFeeConfig,
  getSharedFeeConfig,
  getSharedMint,
  getSharedOpenMint,
  getSharedStakingConfig,
  unit,
} from "./shared-fixtures";

/** `Role::Merchant` — index 0, and the only role this relay draws on. */
const ROLE_MERCHANT_BYTE = 0;
const ROLE_MERCHANT = { merchant: {} };

/**
 * The arbitration deposit these tests charge: 10 OPEN, OFS-4100 §6's
 * signed-off dispute filing fee.
 */
const FILING_FEE = unit(10);

describe("stake recovery relay", () => {
  anchor.setProvider(anchor.AnchorProvider.env());
  const provider = anchor.AnchorProvider.env();
  const connection = provider.connection;

  const escrow = anchor.workspace.escrow as Program<Escrow>;
  const staking = anchor.workspace.staking as Program<Staking>;
  const admin = (provider.wallet as anchor.Wallet).payer;

  let settlementMint: PublicKey;
  let openMint: PublicKey;
  let shared: SharedFeeConfig;
  let feeConfig: PublicKey;
  let arbitrationPool: PublicKey;
  let stakingConfig: PublicKey;
  let stakeVault: PublicKey;
  /**
   * OFS-4100 §4's Merchant floor, read off the shared `StakingConfig`
   * rather than restated here. Every amount below is expressed relative to
   * it, so this suite keeps proving the same things if the fixture's
   * figures ever move.
   */
  let merchantMinStake: BN;

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

  async function mintTokens(mintPk: PublicKey, dest: PublicKey, amount: BN) {
    await mintTo(
      connection,
      admin,
      mintPk,
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

  async function expectAnchorError(send: () => Promise<unknown>, code: string) {
    try {
      await withBlockhashRetry(send);
      expect.fail(`expected instruction to fail with ${code}, but it succeeded`);
    } catch (err: any) {
      const actual = err?.error?.errorCode?.code ?? String(err);
      expect(actual).to.equal(code);
    }
  }

  /**
   * Rewrites the shared `FeeConfig`'s dispute filing fee, leaving every
   * other field at the fixture's own value.
   *
   * Spreads `SHARED_FEE_PARAMS` rather than restating the numbers:
   * `update_fee_config` replaces the whole struct, so a hand-copied
   * literal that drifted would silently change the settlement fee or the
   * allowlist for every suite that runs after this one.
   */
  function setFilingFee(fee: BN) {
    return withBlockhashRetry(() =>
      escrow.methods
        .updateFeeConfig({
          ...SHARED_FEE_PARAMS,
          disputeFilingFee: fee,
          settlementMints: [settlementMint],
        })
        .accountsPartial({
          admin: admin.publicKey,
          feeConfig,
          mint: settlementMint,
          devTreasury: shared.devTreasury,
          ecosystemTreasury: shared.ecosystemTreasury,
          infraTreasury: shared.infraTreasury,
          emergencyReserve: shared.emergencyReserve,
        })
        .rpc({ commitment: "confirmed" }),
    );
  }

  const pda = (seeds: (Buffer | Uint8Array)[], programId: PublicKey) =>
    PublicKey.findProgramAddressSync(seeds, programId)[0];

  const liquidityVaultPda = (merchant: PublicKey, mintPk: PublicKey) =>
    pda([Buffer.from("liquidity_vault"), merchant.toBuffer(), mintPk.toBuffer()], escrow.programId);
  const liquidityTokenVaultPda = (merchant: PublicKey, mintPk: PublicKey) =>
    pda(
      [Buffer.from("liquidity_vault_tokens"), merchant.toBuffer(), mintPk.toBuffer()],
      escrow.programId,
    );
  const claimPda = (merchant: PublicKey, mintPk: PublicKey) =>
    pda(
      [Buffer.from("stake_recovery_claim"), merchant.toBuffer(), mintPk.toBuffer()],
      escrow.programId,
    );
  const receiptPda = (merchant: PublicKey) =>
    pda([Buffer.from("stake_recovery_receipt"), merchant.toBuffer()], staking.programId);
  const stakeAccountPda = (owner: PublicKey) =>
    pda(
      [Buffer.from("stake"), owner.toBuffer(), Buffer.from([ROLE_MERCHANT_BYTE])],
      staking.programId,
    );
  const reservationSeed = (id: number) => new BN(id).toArrayLike(Buffer, "le", 8);
  const tradeEscrowPda = (id: number) =>
    pda([Buffer.from("trade_escrow"), reservationSeed(id)], escrow.programId);
  const tradeEscrowTokenVaultPda = (id: number) =>
    pda([Buffer.from("trade_escrow_tokens"), reservationSeed(id)], escrow.programId);
  const disputeCasePda = (id: number) =>
    pda([Buffer.from("dispute_case"), reservationSeed(id)], escrow.programId);

  before(async function () {
    this.timeout(300000);

    settlementMint = await getSharedMint();
    openMint = await getSharedOpenMint();
    shared = await getSharedFeeConfig(escrow);
    ({ feeConfig, arbitrationPool } = shared);
    ({ stakingConfig, stakeVault } = await getSharedStakingConfig(staking));

    // The agreement the whole relay rests on, asserted rather than
    // assumed. If the fixture ever drifts back to a settlement-denominated
    // stake vault, this fails here with a readable message instead of
    // surfacing as an opaque SPL token error six instructions later.
    const config = await staking.account.stakingConfig.fetch(stakingConfig);
    expect(config.mint.toBase58(), "staked asset must be OPEN").to.equal(openMint.toBase58());
    const pool = await getAccount(connection, arbitrationPool, "confirmed", TOKEN_2022_PROGRAM_ID);
    expect(pool.mint.toBase58(), "arbitration pool must hold OPEN").to.equal(openMint.toBase58());
    expect(settlementMint.toBase58(), "settlement mint must not be OPEN").to.not.equal(
      openMint.toBase58(),
    );

    merchantMinStake = config.minStakeByRole[ROLE_MERCHANT_BYTE];
    await setFilingFee(FILING_FEE);
  });

  // Hand the shared singleton back exactly as the fixture left it. Every
  // other suite opens dispute cases expecting a zero deposit, so a leaked
  // filing fee breaks them, not this file.
  after(async function () {
    this.timeout(300000);
    if (feeConfig) await setFilingFee(SHARED_FEE_PARAMS.disputeFilingFee);
  });

  /**
   * A merchant with a settlement vault, an OPEN vault holding
   * `openFunding`, and a Merchant-role stake of `stakeAmount`.
   */
  async function setUpMerchant(openFunding: BN, stakeAmount: BN) {
    const merchant = Keypair.generate();
    await airdrop(merchant.publicKey);

    for (const mintPk of [settlementMint, openMint]) {
      await withBlockhashRetry(() =>
        escrow.methods
          .createLiquidityVault()
          .accountsPartial({
            merchant: merchant.publicKey,
            mint: mintPk,
            liquidityVault: liquidityVaultPda(merchant.publicKey, mintPk),
            tokenVault: liquidityTokenVaultPda(merchant.publicKey, mintPk),
            tokenProgram: TOKEN_2022_PROGRAM_ID,
            systemProgram: SystemProgram.programId,
            rent: SYSVAR_RENT_PUBKEY,
          })
          .signers([merchant])
          .rpc({ commitment: "confirmed" }),
      );
    }

    if (!openFunding.isZero()) {
      const merchantOpenAta = await ata(openMint, merchant.publicKey);
      await mintTokens(openMint, merchantOpenAta, openFunding);
      await withBlockhashRetry(() =>
        escrow.methods
          .depositLiquidity(openFunding)
          .accountsPartial({
            merchant: merchant.publicKey,
            liquidityVault: liquidityVaultPda(merchant.publicKey, openMint),
            tokenVault: liquidityTokenVaultPda(merchant.publicKey, openMint),
            from: merchantOpenAta,
            mint: openMint,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
          })
          .signers([merchant])
          .rpc({ commitment: "confirmed" }),
      );
    }

    await withBlockhashRetry(() =>
      staking.methods
        .initializeStakeAccount(ROLE_MERCHANT)
        .accountsPartial({
          owner: merchant.publicKey,
          stakeAccount: stakeAccountPda(merchant.publicKey),
          systemProgram: SystemProgram.programId,
        })
        .signers([merchant])
        .rpc({ commitment: "confirmed" }),
    );

    if (!stakeAmount.isZero()) {
      const stakerAta = await ata(openMint, merchant.publicKey);
      await mintTokens(openMint, stakerAta, stakeAmount);
      await withBlockhashRetry(() =>
        staking.methods
          .stake(stakeAmount)
          .accountsPartial({
            owner: merchant.publicKey,
            stakingConfig,
            stakeAccount: stakeAccountPda(merchant.publicKey),
            stakeVault,
            from: stakerAta,
            mint: openMint,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
          })
          .signers([merchant])
          .rpc({ commitment: "confirmed" }),
      );
    }

    return merchant;
  }

  /** Drives a trade to `AwaitingFiatSettlement`, ready to be disputed. */
  async function openFundedTradeEscrow(merchant: Keypair, reservationId: number, amount: BN) {
    const buyer = Keypair.generate();
    await airdrop(buyer.publicKey);

    const liquidityVault = liquidityVaultPda(merchant.publicKey, settlementMint);
    const liquidityTokenVault = liquidityTokenVaultPda(merchant.publicKey, settlementMint);
    const merchantAta = await ata(settlementMint, merchant.publicKey);
    await mintTokens(settlementMint, merchantAta, amount.muln(2));

    await withBlockhashRetry(() =>
      escrow.methods
        .depositLiquidity(amount.muln(2))
        .accountsPartial({
          merchant: merchant.publicKey,
          liquidityVault,
          tokenVault: liquidityTokenVault,
          from: merchantAta,
          mint: settlementMint,
          tokenProgram: TOKEN_2022_PROGRAM_ID,
        })
        .signers([merchant])
        .rpc({ commitment: "confirmed" }),
    );

    await withBlockhashRetry(() =>
      escrow.methods
        .reserveLiquidity(amount)
        .accountsPartial({ merchant: merchant.publicKey, liquidityVault })
        .signers([merchant])
        .rpc({ commitment: "confirmed" }),
    );

    await withBlockhashRetry(() =>
      escrow.methods
        .createTradeEscrow(new BN(reservationId), amount, new BN(1800))
        .accountsPartial({
          merchant: merchant.publicKey,
          buyer: buyer.publicKey,
          mint: settlementMint,
          liquidityVault,
          tradeEscrow: tradeEscrowPda(reservationId),
          tokenVault: tradeEscrowTokenVaultPda(reservationId),
          tokenProgram: TOKEN_2022_PROGRAM_ID,
          systemProgram: SystemProgram.programId,
          rent: SYSVAR_RENT_PUBKEY,
        })
        .signers([merchant])
        .rpc({ commitment: "confirmed" }),
    );

    await withBlockhashRetry(() =>
      escrow.methods
        .fundTradeEscrow()
        .accountsPartial({
          merchant: merchant.publicKey,
          mint: settlementMint,
          liquidityVault,
          liquidityTokenVault,
          tradeEscrow: tradeEscrowPda(reservationId),
          tradeEscrowTokenVault: tradeEscrowTokenVaultPda(reservationId),
          tokenProgram: TOKEN_2022_PROGRAM_ID,
        })
        .signers([merchant])
        .rpc({ commitment: "confirmed" }),
    );

    return buyer;
  }

  async function openDispute(merchant: Keypair, buyer: Keypair, reservationId: number) {
    await withBlockhashRetry(() =>
      escrow.methods
        .openDisputeCase(new BN(60), new BN(60))
        .accountsPartial({
          signer: buyer.publicKey,
          payer: admin.publicKey,
          tradeEscrow: tradeEscrowPda(reservationId),
          disputeCase: disputeCasePda(reservationId),
          feeConfig,
          depositMint: openMint,
          merchantOpenVault: liquidityVaultPda(merchant.publicKey, openMint),
          merchantOpenTokenVault: liquidityTokenVaultPda(merchant.publicKey, openMint),
          arbitrationPool,
          stakeRecoveryClaim: claimPda(merchant.publicKey, openMint),
          tokenProgram: TOKEN_2022_PROGRAM_ID,
          systemProgram: SystemProgram.programId,
          slotHashes: SYSVAR_SLOT_HASHES_PUBKEY,
        })
        .signers([buyer])
        .rpc({ commitment: "confirmed" }),
    );
  }

  function recover(merchant: PublicKey) {
    return staking.methods
      .recoverStakeShortfall()
      .accountsPartial({
        // Deliberately the test wallet rather than the merchant: the
        // instruction is permissionless, and the only signer is a fee
        // payer with no authority over anything.
        payer: admin.publicKey,
        mint: openMint,
        stakingConfig,
        stakeAccount: stakeAccountPda(merchant),
        stakeVault,
        recoveryClaim: claimPda(merchant, openMint),
        recoveryReceipt: receiptPda(merchant),
        merchantOpenTokenVault: liquidityTokenVaultPda(merchant, openMint),
        tokenProgram: TOKEN_2022_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .rpc({ commitment: "confirmed" });
  }

  function absorb(merchant: PublicKey) {
    return escrow.methods
      .absorbStakeRecovery()
      .accountsPartial({
        mint: openMint,
        claim: claimPda(merchant, openMint),
        recoveryReceipt: receiptPda(merchant),
        merchantOpenVault: liquidityVaultPda(merchant, openMint),
        merchantOpenTokenVault: liquidityTokenVaultPda(merchant, openMint),
      })
      .rpc({ commitment: "confirmed" });
  }

  function topUp(merchant: PublicKey, reservationId: number) {
    return escrow.methods
      .topUpArbitrationDeposit()
      .accountsPartial({
        disputeCase: disputeCasePda(reservationId),
        tradeEscrow: tradeEscrowPda(reservationId),
        mint: openMint,
        merchantOpenVault: liquidityVaultPda(merchant, openMint),
        merchantOpenTokenVault: liquidityTokenVaultPda(merchant, openMint),
        arbitrationPool,
        tokenProgram: TOKEN_2022_PROGRAM_ID,
      })
      .rpc({ commitment: "confirmed" });
  }

  function withdraw(merchant: Keypair, to: PublicKey) {
    return staking.methods
      .withdrawUnstaked()
      .accountsPartial({
        owner: merchant.publicKey,
        mint: openMint,
        stakingConfig,
        stakeAccount: stakeAccountPda(merchant.publicKey),
        stakeVault,
        to,
        recoveryClaim: claimPda(merchant.publicKey, openMint),
        recoveryReceipt: receiptPda(merchant.publicKey),
        tokenProgram: TOKEN_2022_PROGRAM_ID,
      })
      .signers([merchant])
      .rpc({ commitment: "confirmed" });
  }

  describe("a merchant whose stake covers the debt", () => {
    const RESERVATION = 77001;
    // Three of the ten OPEN the filing fee costs, so the case opens seven
    // short — a partial collection, which is the case that actually
    // exercises the relay.
    const VAULT_FUNDING = unit(3);
    const SHORTFALL = FILING_FEE.sub(VAULT_FUNDING);
    let merchant: Keypair;
    let buyer: Keypair;

    before(async function () {
      this.timeout(300000);
      merchant = await setUpMerchant(VAULT_FUNDING, merchantMinStake);
      buyer = await openFundedTradeEscrow(merchant, RESERVATION, unit(100));
      await openDispute(merchant, buyer, RESERVATION);
    });

    it("opens the case anyway and records the uncovered part as a debt", async () => {
      const disputeCase = await escrow.account.disputeCase.fetch(disputeCasePda(RESERVATION));
      expect(disputeCase.deposit.toString()).to.equal(VAULT_FUNDING.toString());
      expect(disputeCase.depositShortfall.toString()).to.equal(SHORTFALL.toString());

      const claim = await escrow.account.stakeRecoveryClaim.fetch(
        claimPda(merchant.publicKey, openMint),
      );
      expect(claim.merchant.toBase58()).to.equal(merchant.publicKey.toBase58());
      expect(claim.mint.toBase58()).to.equal(openMint.toBase58());
      expect(claim.owedTotal.toString()).to.equal(SHORTFALL.toString());
      expect(claim.creditedTotal.toString()).to.equal("0");
      expect(claim.caseCount).to.equal(1);
    });

    it("refuses to release unbonded stake while the debt stands", async () => {
      await withBlockhashRetry(() =>
        staking.methods
          .requestUnstake(merchantMinStake)
          .accountsPartial({
            owner: merchant.publicKey,
            stakingConfig,
            stakeAccount: stakeAccountPda(merchant.publicKey),
          })
          .signers([merchant])
          .rpc({ commitment: "confirmed" }),
      );
      await new Promise((r) => setTimeout(r, 1500));

      // The whole point. Without this the merchant walks the entire stake
      // out from under a debt they can see coming, and every other part of
      // the relay is decoration.
      const to = await ata(openMint, merchant.publicKey);
      await expectAnchorError(() => withdraw(merchant, to), "StakeRecoveryOutstanding");
    });

    it("takes the debt out of the unbonding cohort first, and pays it into the merchant's own vault", async () => {
      const vaultTokens = liquidityTokenVaultPda(merchant.publicKey, openMint);
      const before = await getAccount(connection, vaultTokens, "confirmed", TOKEN_2022_PROGRAM_ID);

      await withBlockhashRetry(() => recover(merchant.publicKey));

      const stakeAccount = await staking.account.stakeAccount.fetch(
        stakeAccountPda(merchant.publicKey),
      );
      // Everything was unbonding, so the active balance is untouched and
      // the cohort shrank by exactly the debt.
      expect(stakeAccount.amount.toString()).to.equal("0");
      expect(stakeAccount.unbondingAmount.toString()).to.equal(
        merchantMinStake.sub(SHORTFALL).toString(),
      );

      const receipt = await staking.account.stakeRecoveryReceipt.fetch(
        receiptPda(merchant.publicKey),
      );
      expect(receipt.merchant.toBase58()).to.equal(merchant.publicKey.toBase58());
      expect(receipt.recoveredTotal.toString()).to.equal(SHORTFALL.toString());
      expect(receipt.recoveryCount).to.equal(1);

      const after = await getAccount(connection, vaultTokens, "confirmed", TOKEN_2022_PROGRAM_ID);
      expect((after.amount - before.amount).toString()).to.equal(SHORTFALL.toString());
    });

    it("refuses a second recovery once the debt is settled", async () => {
      await expectAnchorError(() => recover(merchant.publicKey), "NothingToRecover");
    });

    it("credits the recovered tokens to the vault's own accounting", async () => {
      const vault = liquidityVaultPda(merchant.publicKey, openMint);
      const before = await escrow.account.liquidityVault.fetch(vault);
      // The tokens are in the token account but not yet in the counters —
      // the discrepancy `absorb_stake_recovery` exists to close.
      expect(before.available.toString()).to.equal("0");

      await withBlockhashRetry(() => absorb(merchant.publicKey));

      const after = await escrow.account.liquidityVault.fetch(vault);
      expect(after.available.toString()).to.equal(SHORTFALL.toString());
      expect(after.total.toString()).to.equal(SHORTFALL.toString());

      const claim = await escrow.account.stakeRecoveryClaim.fetch(
        claimPda(merchant.publicKey, openMint),
      );
      expect(claim.creditedTotal.toString()).to.equal(SHORTFALL.toString());
    });

    it("refuses a second absorb, which would credit liquidity twice", async () => {
      await expectAnchorError(() => absorb(merchant.publicKey), "NothingToAbsorb");
    });

    it("puts that liquidity into the arbitration pool and makes the deposit whole", async () => {
      const poolBefore = await getAccount(
        connection,
        arbitrationPool,
        "confirmed",
        TOKEN_2022_PROGRAM_ID,
      );

      await withBlockhashRetry(() => topUp(merchant.publicKey, RESERVATION));

      const disputeCase = await escrow.account.disputeCase.fetch(disputeCasePda(RESERVATION));
      expect(disputeCase.deposit.toString()).to.equal(FILING_FEE.toString());
      expect(disputeCase.depositShortfall.toString()).to.equal("0");

      const poolAfter = await getAccount(
        connection,
        arbitrationPool,
        "confirmed",
        TOKEN_2022_PROGRAM_ID,
      );
      expect((poolAfter.amount - poolBefore.amount).toString()).to.equal(SHORTFALL.toString());
    });

    it("refuses a top-up once the deposit is whole", async () => {
      await expectAnchorError(() => topUp(merchant.publicKey, RESERVATION), "NoDepositShortfall");
    });

    it("releases the remaining unbonded stake now that nothing is owed", async () => {
      const to = await ata(openMint, merchant.publicKey);
      const before = await getAccount(connection, to, "confirmed", TOKEN_2022_PROGRAM_ID);

      await withBlockhashRetry(() => withdraw(merchant, to));

      const after = await getAccount(connection, to, "confirmed", TOKEN_2022_PROGRAM_ID);
      expect((after.amount - before.amount).toString()).to.equal(
        merchantMinStake.sub(SHORTFALL).toString(),
      );
    });
  });

  describe("a merchant whose stake does not cover the debt", () => {
    const RESERVATION = 77002;
    // A filing fee above the Merchant floor, so one case creates a debt
    // the merchant's entire stake cannot clear. This is the corner
    // OFS-4100's status banner records as an open design question: what
    // happens when the *stake* is also insufficient.
    let bigFee: BN;
    let unrecoverable: BN;
    let merchant: Keypair;

    before(async function () {
      this.timeout(300000);
      bigFee = merchantMinStake.add(unit(100));
      unrecoverable = bigFee.sub(merchantMinStake);
      await setFilingFee(bigFee);

      // No OPEN in the vault at all: the deposit cannot be collected even
      // in part, so the whole filing fee becomes a debt. An empty vault is
      // exactly the position §9.3 says must not make a merchant
      // undisputable.
      merchant = await setUpMerchant(new BN(0), merchantMinStake);
      const buyer = await openFundedTradeEscrow(merchant, RESERVATION, unit(50));
      await openDispute(merchant, buyer, RESERVATION);
    });

    it("opens the case against an empty vault and books the whole fee as debt", async () => {
      const disputeCase = await escrow.account.disputeCase.fetch(disputeCasePda(RESERVATION));
      expect(disputeCase.deposit.toString()).to.equal("0");
      expect(disputeCase.depositShortfall.toString()).to.equal(bigFee.toString());

      const claim = await escrow.account.stakeRecoveryClaim.fetch(
        claimPda(merchant.publicKey, openMint),
      );
      expect(claim.owedTotal.toString()).to.equal(bigFee.toString());
    });

    it("takes everything the stake holds rather than refusing a partial payment", async () => {
      await withBlockhashRetry(() => recover(merchant.publicKey));

      const stakeAccount = await staking.account.stakeAccount.fetch(
        stakeAccountPda(merchant.publicKey),
      );
      expect(stakeAccount.amount.toString()).to.equal("0");
      // Emptied, so the age clock stops — the same invariant `slash`
      // maintains, and what stops a re-staked position presenting an age
      // it held no capital through.
      expect(stakeAccount.firstStakedAt.toString()).to.equal("0");

      const receipt = await staking.account.stakeRecoveryReceipt.fetch(
        receiptPda(merchant.publicKey),
      );
      expect(receipt.recoveredTotal.toString()).to.equal(merchantMinStake.toString());
    });

    it("leaves the remainder outstanding on the claim rather than writing it off", async () => {
      const claim = await escrow.account.stakeRecoveryClaim.fetch(
        claimPda(merchant.publicKey, openMint),
      );
      const receipt = await staking.account.stakeRecoveryReceipt.fetch(
        receiptPda(merchant.publicKey),
      );
      // The debt outlives the stake that could not pay it. Both counters
      // are monotone, so what is still owed is always their difference —
      // and it is still 100 OPEN.
      expect(claim.owedTotal.sub(receipt.recoveredTotal).toString()).to.equal(
        unrecoverable.toString(),
      );
    });

    it("says so plainly when there is nothing left to take", async () => {
      await expectAnchorError(() => recover(merchant.publicKey), "NoStakeToRecoverFrom");
    });

    it("leaves the case visibly short after the top-up, not silently settled", async () => {
      await withBlockhashRetry(() => absorb(merchant.publicKey));
      await withBlockhashRetry(() => topUp(merchant.publicKey, RESERVATION));

      const disputeCase = await escrow.account.disputeCase.fetch(disputeCasePda(RESERVATION));
      expect(disputeCase.deposit.toString()).to.equal(merchantMinStake.toString());
      // The arbitrators on this case will be 100 OPEN short of a full
      // reward, and the account says so. A partial payout that presented
      // itself as complete is the outcome this whole design exists to
      // avoid.
      expect(disputeCase.depositShortfall.toString()).to.equal(unrecoverable.toString());
    });

    it("refuses a further top-up once the vault is empty again", async () => {
      await expectAnchorError(
        () => topUp(merchant.publicKey, RESERVATION),
        "NoLiquidityForShortfall",
      );
    });
  });

  describe("a merchant who owes nothing", () => {
    it("cannot have stake taken, and can withdraw freely", async function () {
      this.timeout(300000);
      const merchant = await setUpMerchant(new BN(0), merchantMinStake);

      // No case has ever opened against them, so no claim account exists.
      // Absence must read as "owes nothing" rather than as an error, or
      // every honest merchant would need an account they have no reason to
      // create.
      expect(await connection.getAccountInfo(claimPda(merchant.publicKey, openMint))).to.equal(null);
      await expectAnchorError(() => recover(merchant.publicKey), "NothingToRecover");

      await withBlockhashRetry(() =>
        staking.methods
          .requestUnstake(merchantMinStake)
          .accountsPartial({
            owner: merchant.publicKey,
            stakingConfig,
            stakeAccount: stakeAccountPda(merchant.publicKey),
          })
          .signers([merchant])
          .rpc({ commitment: "confirmed" }),
      );
      await new Promise((r) => setTimeout(r, 1500));

      const to = await ata(openMint, merchant.publicKey);
      const before = await getAccount(connection, to, "confirmed", TOKEN_2022_PROGRAM_ID);
      await withBlockhashRetry(() => withdraw(merchant, to));
      const after = await getAccount(connection, to, "confirmed", TOKEN_2022_PROGRAM_ID);
      expect((after.amount - before.amount).toString()).to.equal(merchantMinStake.toString());
    });
  });
});
