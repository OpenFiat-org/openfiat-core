import * as anchor from "@anchor-lang/core";
import { Program, BN } from "@anchor-lang/core";
import { Escrow } from "../target/types/escrow";
import { Staking } from "../target/types/staking";
import * as crypto from "crypto";
import {
  TOKEN_2022_PROGRAM_ID,
  mintTo,
  getOrCreateAssociatedTokenAccount,
  getAccount,
  createMint,
  createTransferCheckedInstruction,
} from "@solana/spl-token";
import {
  Keypair,
  PublicKey,
  SystemProgram,
  SYSVAR_RENT_PUBKEY,
  Transaction,
} from "@solana/web3.js";
import { expect } from "chai";
import { getSharedMint, getSharedFeeConfig, getSharedStakingConfig, unit, MINT_DECIMALS } from "./shared-fixtures";

describe("escrow", () => {
  anchor.setProvider(anchor.AnchorProvider.env());
  const provider = anchor.AnchorProvider.env();
  const connection = provider.connection;

  const program = anchor.workspace.escrow as Program<Escrow>;
  const admin = (provider.wallet as anchor.Wallet).payer;

  let mint: PublicKey;
  let feeConfig: PublicKey;
  let devTreasury: PublicKey;
  let ecosystemTreasury: PublicKey;
  let infraTreasury: PublicKey;
  let emergencyReserve: PublicKey;

  async function airdrop(pubkey: PublicKey, sol = 10) {
    const sig = await connection.requestAirdrop(pubkey, sol * 1_000_000_000);
    const latest = await connection.getLatestBlockhash();
    await connection.confirmTransaction({ signature: sig, ...latest });
  }

  async function ata(mintPk: PublicKey, owner: PublicKey, allowOwnerOffCurve = false) {
    const acc = await getOrCreateAssociatedTokenAccount(
      connection,
      admin,
      mintPk,
      owner,
      allowOwnerOffCurve,
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

  /**
   * Retries a transaction send on a transient "Blockhash not found"
   * simulation error — same local-validator/RPC race `presale.ts`
   * already documents and mitigates.
   */
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

  function liquidityVaultPda(merchant: PublicKey, mintPk: PublicKey) {
    return PublicKey.findProgramAddressSync(
      [Buffer.from("liquidity_vault"), merchant.toBuffer(), mintPk.toBuffer()],
      program.programId,
    )[0];
  }
  function liquidityTokenVaultPda(merchant: PublicKey, mintPk: PublicKey) {
    return PublicKey.findProgramAddressSync(
      [Buffer.from("liquidity_vault_tokens"), merchant.toBuffer(), mintPk.toBuffer()],
      program.programId,
    )[0];
  }
  function tradeEscrowPda(reservationId: number) {
    return PublicKey.findProgramAddressSync(
      [Buffer.from("trade_escrow"), new BN(reservationId).toArrayLike(Buffer, "le", 8)],
      program.programId,
    )[0];
  }
  function tradeEscrowTokenVaultPda(reservationId: number) {
    return PublicKey.findProgramAddressSync(
      [Buffer.from("trade_escrow_tokens"), new BN(reservationId).toArrayLike(Buffer, "le", 8)],
      program.programId,
    )[0];
  }
  before(async () => {
    mint = await getSharedMint();
    ({ feeConfig, devTreasury, ecosystemTreasury, infraTreasury, emergencyReserve } =
      await getSharedFeeConfig(program));
  });

  describe("update_fee_config", () => {
    // The deployed devnet FeeConfig was initialized with the treasury
    // *owner* wallets instead of their token accounts, which made every
    // release_escrow un-executable (Anchor cannot deserialize a wallet as
    // a TokenAccount). These cover the instruction that corrects it, and
    // the constraints that stop the same mistake being stored again.
    const ORIGINAL = {
      adListingFee: new BN(0),
      disputeFilingFee: new BN(0),
      settlementFeeBps: 15,
      devTreasuryBps: 4000,
      ecosystemTreasuryBps: 3000,
      infraTreasuryBps: 2000,
      emergencyReserveBps: 1000,
      timeoutSecs: new BN(1800),
    };

    // Restore the shared singleton so later specs see the fixture's own
    // treasuries, whatever this block did to it.
    afterEach(async () => {
      await withBlockhashRetry(() =>
        program.methods
          .updateFeeConfig(ORIGINAL)
          .accountsPartial({
            admin: admin.publicKey,
            feeConfig,
            mint,
            devTreasury,
            ecosystemTreasury,
            infraTreasury,
            emergencyReserve,
          })
          .rpc({ commitment: "confirmed" }),
      );
    });

    it("lets the admin repoint the treasuries at different token accounts", async () => {
      const newDev = await ata(mint, Keypair.generate().publicKey);
      const newEco = await ata(mint, Keypair.generate().publicKey);

      await withBlockhashRetry(() =>
        program.methods
          .updateFeeConfig({ ...ORIGINAL, settlementFeeBps: 25 })
          .accountsPartial({
            admin: admin.publicKey,
            feeConfig,
            mint,
            devTreasury: newDev,
            ecosystemTreasury: newEco,
            infraTreasury,
            emergencyReserve,
          })
          .rpc({ commitment: "confirmed" }),
      );

      const cfg = await program.account.feeConfig.fetch(feeConfig);
      expect(cfg.devTreasury.toBase58()).to.equal(newDev.toBase58());
      expect(cfg.ecosystemTreasury.toBase58()).to.equal(newEco.toBase58());
      expect(cfg.settlementFeeBps).to.equal(25);
      // Not updatable here, by design.
      expect(cfg.admin.toBase58()).to.equal(admin.publicKey.toBase58());
    });

    it("rejects a non-admin signer", async () => {
      const intruder = Keypair.generate();
      await airdrop(intruder.publicKey, 1);

      await expectAnchorError(
        withBlockhashRetry(() =>
          program.methods
            .updateFeeConfig(ORIGINAL)
            .accountsPartial({
              admin: intruder.publicKey,
              feeConfig,
              mint,
              devTreasury,
              ecosystemTreasury,
              infraTreasury,
              emergencyReserve,
            })
            .signers([intruder])
            .rpc({ commitment: "confirmed" }),
        ),
        "Unauthorized",
      );
    });

    it("rejects splits that do not sum to 10_000", async () => {
      await expectAnchorError(
        withBlockhashRetry(() =>
          program.methods
            .updateFeeConfig({ ...ORIGINAL, devTreasuryBps: 4001 })
            .accountsPartial({
              admin: admin.publicKey,
              feeConfig,
              mint,
              devTreasury,
              ecosystemTreasury,
              infraTreasury,
              emergencyReserve,
            })
            .rpc({ commitment: "confirmed" }),
        ),
        "InvalidFeeSplit",
      );
    });

    it("refuses a wallet address where a treasury token account is required", async () => {
      // The exact defect that broke the live deployment: a plain owner
      // pubkey cannot deserialize as a TokenAccount, so the runtime
      // rejects it instead of storing an unusable config.
      const walletNotTokenAccount = Keypair.generate().publicKey;
      let failed = false;
      try {
        await program.methods
          .updateFeeConfig(ORIGINAL)
          .accountsPartial({
            admin: admin.publicKey,
            feeConfig,
            mint,
            devTreasury: walletNotTokenAccount,
            ecosystemTreasury,
            infraTreasury,
            emergencyReserve,
          })
          .rpc({ commitment: "confirmed" });
      } catch {
        failed = true;
      }
      expect(failed, "storing a wallet as a treasury must fail").to.equal(true);
    });

    it("refuses a token account for a different mint", async () => {
      const otherMint = await createMint(
        connection,
        admin,
        admin.publicKey,
        null,
        MINT_DECIMALS,
        undefined,
        { commitment: "confirmed" },
        TOKEN_2022_PROGRAM_ID,
      );
      const wrongMintTreasury = await ata(otherMint, Keypair.generate().publicKey);

      let failed = false;
      try {
        await program.methods
          .updateFeeConfig(ORIGINAL)
          .accountsPartial({
            admin: admin.publicKey,
            feeConfig,
            mint,
            devTreasury: wrongMintTreasury,
            ecosystemTreasury,
            infraTreasury,
            emergencyReserve,
          })
          .rpc({ commitment: "confirmed" });
      } catch {
        failed = true;
      }
      expect(failed, "a wrong-mint treasury must fail").to.equal(true);
    });
  });

  describe("full liquidity -> reserve -> escrow -> release cycle", () => {
    let merchant: Keypair;
    let buyer: Keypair;
    let liquidityVault: PublicKey;
    let liquidityTokenVault: PublicKey;
    const reservationId = 1;
    const amount = unit(1000);

    before(async () => {
      merchant = Keypair.generate();
      buyer = Keypair.generate();
      await airdrop(merchant.publicKey);
      await airdrop(buyer.publicKey);

      liquidityVault = liquidityVaultPda(merchant.publicKey, mint);
      liquidityTokenVault = liquidityTokenVaultPda(merchant.publicKey, mint);

      await withBlockhashRetry(() =>
        program.methods
        .createLiquidityVault()
        .accountsPartial({
          merchant: merchant.publicKey,
          mint,
          liquidityVault,
          tokenVault: liquidityTokenVault,
          tokenProgram: TOKEN_2022_PROGRAM_ID,
          systemProgram: SystemProgram.programId,
          rent: SYSVAR_RENT_PUBKEY,
        })
        .signers([merchant])
        .rpc({ commitment: "confirmed" }),
      );

      const merchantAta = await ata(mint, merchant.publicKey);
      await mintTokens(merchantAta, unit(5000));

      await withBlockhashRetry(() =>
        program.methods
        .depositLiquidity(unit(5000))
        .accountsPartial({
          merchant: merchant.publicKey,
          liquidityVault,
          tokenVault: liquidityTokenVault,
          from: merchantAta,
          mint,
          tokenProgram: TOKEN_2022_PROGRAM_ID,
        })
        .signers([merchant])
        .rpc({ commitment: "confirmed" }),
      );
    });

    it("reserves liquidity as a counter-only marking (no transfer)", async () => {
      await withBlockhashRetry(() =>
        program.methods
        .reserveLiquidity(amount)
        .accountsPartial({ merchant: merchant.publicKey, liquidityVault })
        .signers([merchant])
        .rpc({ commitment: "confirmed" }),
      );

      const vault = await program.account.liquidityVault.fetch(liquidityVault);
      expect(vault.reserved.toString()).to.equal(amount.toString());
      expect(vault.available.toString()).to.equal(unit(4000).toString());
      const tokenAccount = await getAccount(
        connection,
        liquidityTokenVault,
        "confirmed",
        TOKEN_2022_PROGRAM_ID,
      );
      expect(tokenAccount.amount.toString()).to.equal(unit(5000).toString());
    });

    it("creates and funds a trade escrow, moving tokens out of the liquidity vault", async () => {
      const tradeEscrow = tradeEscrowPda(reservationId);
      const tradeEscrowTokenVault = tradeEscrowTokenVaultPda(reservationId);

      await withBlockhashRetry(() =>
        program.methods
        .createTradeEscrow(new BN(reservationId), amount, new BN(1800))
        .accountsPartial({
          merchant: merchant.publicKey,
          buyer: buyer.publicKey,
          mint,
          liquidityVault,
          tradeEscrow,
          tokenVault: tradeEscrowTokenVault,
          tokenProgram: TOKEN_2022_PROGRAM_ID,
          systemProgram: SystemProgram.programId,
          rent: SYSVAR_RENT_PUBKEY,
        })
        .signers([merchant])
        .rpc({ commitment: "confirmed" }),
      );

      await withBlockhashRetry(() =>
        program.methods
        .fundTradeEscrow()
        .accountsPartial({
          merchant: merchant.publicKey,
          mint,
          liquidityVault,
          liquidityTokenVault,
          tradeEscrow,
          tradeEscrowTokenVault,
          tokenProgram: TOKEN_2022_PROGRAM_ID,
        })
        .signers([merchant])
        .rpc({ commitment: "confirmed" }),
      );

      const vault = await program.account.liquidityVault.fetch(liquidityVault);
      expect(vault.reserved.toString()).to.equal("0");
      expect(vault.pendingSettlement.toString()).to.equal(amount.toString());

      const escrowAccount = await program.account.tradeEscrowVault.fetch(tradeEscrow);
      expect(escrowAccount.state).to.deep.equal({ awaitingFiatSettlement: {} });

      const escrowTokens = await getAccount(
        connection,
        tradeEscrowTokenVault,
        "confirmed",
        TOKEN_2022_PROGRAM_ID,
      );
      expect(escrowTokens.amount.toString()).to.equal(amount.toString());
    });

    it("rejects release_escrow before approve_settlement has run", async () => {
      const tradeEscrow = tradeEscrowPda(reservationId);
      const tradeEscrowTokenVault = tradeEscrowTokenVaultPda(reservationId);
      const buyerAta = await ata(mint, buyer.publicKey);

      await expectAnchorError(
        program.methods
          .releaseEscrow()
          .accountsPartial({
            mint,
            liquidityVault,
            tradeEscrow,
            tradeEscrowTokenVault,
            buyerTokenAccount: buyerAta,
            feeConfig,
            devTreasury,
            ecosystemTreasury,
            infraTreasury,
            emergencyReserve,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
          })
          .rpc({ commitment: "confirmed" }),
        "InvalidVaultState",
      );
    });

    it("approves settlement then releases escrow, splitting the settlement fee across treasuries", async () => {
      const tradeEscrow = tradeEscrowPda(reservationId);
      const tradeEscrowTokenVault = tradeEscrowTokenVaultPda(reservationId);
      const buyerAta = await ata(mint, buyer.publicKey);

      await withBlockhashRetry(() =>
        program.methods
        .approveSettlement()
        .accountsPartial({ merchant: merchant.publicKey, tradeEscrow })
        .signers([merchant])
        .rpc({ commitment: "confirmed" }),
      );

      // Permissionless: no signer beyond the default provider fee-payer is
      // required, demonstrating neither party needs to sign this step.
      await withBlockhashRetry(() =>
        program.methods
        .releaseEscrow()
        .accountsPartial({
          mint,
          liquidityVault,
          tradeEscrow,
          tradeEscrowTokenVault,
          buyerTokenAccount: buyerAta,
          feeConfig,
          devTreasury,
          ecosystemTreasury,
          infraTreasury,
          emergencyReserve,
          tokenProgram: TOKEN_2022_PROGRAM_ID,
        })
        .rpc({ commitment: "confirmed" }),
      );

      const escrowAccount = await program.account.tradeEscrowVault.fetch(tradeEscrow);
      expect(escrowAccount.state).to.deep.equal({ released: {} });

      // fee = 1000 * 15bps = 1.5 units -> 1_500_000 base units at 6 decimals
      const feeBaseUnits = amount.mul(new BN(15)).div(new BN(10_000));
      const buyerExpected = amount.sub(feeBaseUnits);

      const buyerAccount = await getAccount(connection, buyerAta, "confirmed", TOKEN_2022_PROGRAM_ID);
      expect(buyerAccount.amount.toString()).to.equal(buyerExpected.toString());

      const devAccount = await getAccount(connection, devTreasury, "confirmed", TOKEN_2022_PROGRAM_ID);
      const ecosystemAccount = await getAccount(connection, ecosystemTreasury, "confirmed", TOKEN_2022_PROGRAM_ID);
      const infraAccount = await getAccount(connection, infraTreasury, "confirmed", TOKEN_2022_PROGRAM_ID);
      const emergencyAccount = await getAccount(connection, emergencyReserve, "confirmed", TOKEN_2022_PROGRAM_ID);
      const collected =
        BigInt(devAccount.amount) +
        BigInt(ecosystemAccount.amount) +
        BigInt(infraAccount.amount) +
        BigInt(emergencyAccount.amount);
      expect(collected.toString()).to.equal(feeBaseUnits.toString());

      const vault = await program.account.liquidityVault.fetch(liquidityVault);
      expect(vault.pendingSettlement.toString()).to.equal("0");
      expect(vault.settled.toString()).to.equal(amount.toString());
    });

    it("cannot move funds out of the (now-Released) escrow's token vault except via the program's own instructions", async () => {
      const tradeEscrowTokenVault = tradeEscrowTokenVaultPda(reservationId);
      const attackerAta = await ata(mint, buyer.publicKey);

      // A direct transfer_checked, "signed" by an ordinary wallet rather
      // than invoke_signed with the trade_escrow PDA's own seeds, must be
      // rejected — the vault's token-account authority is the PDA, which
      // no external keypair can produce a valid signature for.
      const ix = createTransferCheckedInstruction(
        tradeEscrowTokenVault,
        mint,
        attackerAta,
        buyer.publicKey, // wrong authority: not the trade_escrow PDA
        1,
        MINT_DECIMALS,
        [],
        TOKEN_2022_PROGRAM_ID,
      );
      const tx = new Transaction().add(ix);
      let failed = false;
      try {
        await provider.sendAndConfirm(tx, [buyer], { commitment: "confirmed" });
      } catch {
        failed = true;
      }
      expect(failed).to.equal(true);
    });
  });

  describe("expire_reservation", () => {
    it("returns a funded-but-unapproved escrow's tokens to the liquidity vault once timeout_at has passed", async () => {
      const merchant = Keypair.generate();
      const buyer = Keypair.generate();
      await airdrop(merchant.publicKey);
      await airdrop(buyer.publicKey);

      const liquidityVault = liquidityVaultPda(merchant.publicKey, mint);
      const liquidityTokenVault = liquidityTokenVaultPda(merchant.publicKey, mint);
      const reservationId = 2;
      const amount = unit(200);

      await withBlockhashRetry(() =>
        program.methods
        .createLiquidityVault()
        .accountsPartial({
          merchant: merchant.publicKey,
          mint,
          liquidityVault,
          tokenVault: liquidityTokenVault,
          tokenProgram: TOKEN_2022_PROGRAM_ID,
          systemProgram: SystemProgram.programId,
          rent: SYSVAR_RENT_PUBKEY,
        })
        .signers([merchant])
        .rpc({ commitment: "confirmed" }),
      );

      const merchantAta = await ata(mint, merchant.publicKey);
      await mintTokens(merchantAta, unit(1000));
      await withBlockhashRetry(() =>
        program.methods
        .depositLiquidity(unit(1000))
        .accountsPartial({
          merchant: merchant.publicKey,
          liquidityVault,
          tokenVault: liquidityTokenVault,
          from: merchantAta,
          mint,
          tokenProgram: TOKEN_2022_PROGRAM_ID,
        })
        .signers([merchant])
        .rpc({ commitment: "confirmed" }),
      );

      await withBlockhashRetry(() =>
        program.methods
        .reserveLiquidity(amount)
        .accountsPartial({ merchant: merchant.publicKey, liquidityVault })
        .signers([merchant])
        .rpc({ commitment: "confirmed" }),
      );

      const tradeEscrow = tradeEscrowPda(reservationId);
      const tradeEscrowTokenVault = tradeEscrowTokenVaultPda(reservationId);

      // A 1-second timeout so the test doesn't need to wait 30 real minutes.
      await withBlockhashRetry(() =>
        program.methods
        .createTradeEscrow(new BN(reservationId), amount, new BN(1))
        .accountsPartial({
          merchant: merchant.publicKey,
          buyer: buyer.publicKey,
          mint,
          liquidityVault,
          tradeEscrow,
          tokenVault: tradeEscrowTokenVault,
          tokenProgram: TOKEN_2022_PROGRAM_ID,
          systemProgram: SystemProgram.programId,
          rent: SYSVAR_RENT_PUBKEY,
        })
        .signers([merchant])
        .rpc({ commitment: "confirmed" }),
      );

      await withBlockhashRetry(() =>
        program.methods
        .fundTradeEscrow()
        .accountsPartial({
          merchant: merchant.publicKey,
          mint,
          liquidityVault,
          liquidityTokenVault,
          tradeEscrow,
          tradeEscrowTokenVault,
          tokenProgram: TOKEN_2022_PROGRAM_ID,
        })
        .signers([merchant])
        .rpc({ commitment: "confirmed" }),
      );

      await expectAnchorError(
        program.methods
          .expireReservation()
          .accountsPartial({
            mint,
            liquidityVault,
            tradeEscrow,
            tradeEscrowTokenVault,
            liquidityTokenVault,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
          })
          .rpc({ commitment: "confirmed" }),
        "NotYetExpired",
      );

      await new Promise((r) => setTimeout(r, 2000));

      await withBlockhashRetry(() =>
        program.methods
        .expireReservation()
        .accountsPartial({
          mint,
          liquidityVault,
          tradeEscrow,
          tradeEscrowTokenVault,
          liquidityTokenVault,
          tokenProgram: TOKEN_2022_PROGRAM_ID,
        })
        .rpc({ commitment: "confirmed" }),
      );

      const escrowAccount = await program.account.tradeEscrowVault.fetch(tradeEscrow);
      expect(escrowAccount.state).to.deep.equal({ cancelled: {} });

      const vault = await program.account.liquidityVault.fetch(liquidityVault);
      expect(vault.pendingSettlement.toString()).to.equal("0");
      expect(vault.available.toString()).to.equal(unit(1000).toString());
    });
  });

  describe("dispute-to-chain bridge (Phase 4b)", () => {
    const staking = anchor.workspace.staking as Program<Staking>;
    const ROLE_ARBITRATOR = { arbitrator: {} };
    const OUTCOME_BUYER_WINS = { buyerWins: {} };
    const OUTCOME_MERCHANT_WINS = { merchantWins: {} };
    const OUTCOME_BYTE = { buyerWins: 0, merchantWins: 1, mutualSettlement: 2, invalidDispute: 3 };

    let stakingConfig: PublicKey;
    let stakeVault: PublicKey;
    let rewardsVault: PublicKey;

    function tradeEscrowSeed(reservationId: number) {
      return tradeEscrowPda(reservationId);
    }
    function disputeCasePda(reservationId: number) {
      return PublicKey.findProgramAddressSync(
        [Buffer.from("dispute_case"), new BN(reservationId).toArrayLike(Buffer, "le", 8)],
        program.programId,
      )[0];
    }
    function stakeAccountPda(owner: PublicKey) {
      return PublicKey.findProgramAddressSync(
        [Buffer.from("stake"), owner.toBuffer(), Buffer.from([1])], // Arbitrator = index 1
        staking.programId,
      )[0];
    }
    function commitmentFor(outcomeByte: number, salt: Buffer): Buffer {
      return crypto.createHash("sha256").update(Buffer.from([outcomeByte])).update(salt).digest();
    }

    async function setUpArbitrator(stakeAmount: BN): Promise<Keypair> {
      const owner = Keypair.generate();
      await airdrop(owner.publicKey);
      const stakeAccount = stakeAccountPda(owner.publicKey);

      await withBlockhashRetry(() =>
        staking.methods
          .initializeStakeAccount(ROLE_ARBITRATOR)
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

    async function openFundedTradeEscrow(
      reservationId: number,
      amount: BN,
    ): Promise<{ merchant: Keypair; buyer: Keypair; liquidityVault: PublicKey }> {
      const merchant = Keypair.generate();
      const buyer = Keypair.generate();
      await airdrop(merchant.publicKey);
      await airdrop(buyer.publicKey);

      const liquidityVault = liquidityVaultPda(merchant.publicKey, mint);
      const liquidityTokenVault = liquidityTokenVaultPda(merchant.publicKey, mint);

      await withBlockhashRetry(() =>
        program.methods
          .createLiquidityVault()
          .accountsPartial({
            merchant: merchant.publicKey,
            mint,
            liquidityVault,
            tokenVault: liquidityTokenVault,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
            systemProgram: SystemProgram.programId,
            rent: SYSVAR_RENT_PUBKEY,
          })
          .signers([merchant])
          .rpc({ commitment: "confirmed" }),
      );

      const merchantAta = await ata(mint, merchant.publicKey);
      await mintTokens(merchantAta, amount.muln(2));
      await withBlockhashRetry(() =>
        program.methods
          .depositLiquidity(amount.muln(2))
          .accountsPartial({
            merchant: merchant.publicKey,
            liquidityVault,
            tokenVault: liquidityTokenVault,
            from: merchantAta,
            mint,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
          })
          .signers([merchant])
          .rpc({ commitment: "confirmed" }),
      );

      await withBlockhashRetry(() =>
        program.methods
          .reserveLiquidity(amount)
          .accountsPartial({ merchant: merchant.publicKey, liquidityVault })
          .signers([merchant])
          .rpc({ commitment: "confirmed" }),
      );

      const tradeEscrow = tradeEscrowSeed(reservationId);
      const tradeEscrowTokenVault = tradeEscrowTokenVaultPda(reservationId);

      await withBlockhashRetry(() =>
        program.methods
          .createTradeEscrow(new BN(reservationId), amount, new BN(1800))
          .accountsPartial({
            merchant: merchant.publicKey,
            buyer: buyer.publicKey,
            mint,
            liquidityVault,
            tradeEscrow,
            tokenVault: tradeEscrowTokenVault,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
            systemProgram: SystemProgram.programId,
            rent: SYSVAR_RENT_PUBKEY,
          })
          .signers([merchant])
          .rpc({ commitment: "confirmed" }),
      );

      await withBlockhashRetry(() =>
        program.methods
          .fundTradeEscrow()
          .accountsPartial({
            merchant: merchant.publicKey,
            mint,
            liquidityVault,
            liquidityTokenVault,
            tradeEscrow,
            tradeEscrowTokenVault,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
          })
          .signers([merchant])
          .rpc({ commitment: "confirmed" }),
      );

      return { merchant, buyer, liquidityVault };
    }

    before(async () => {
      ({ stakingConfig, stakeVault, rewardsVault } = await getSharedStakingConfig(staking));
    });

    it("tallies a stake-weighted majority (BuyerWins) and releases funds accordingly", async () => {
      const reservationId = 9001;
      const amount = unit(1000);
      const { merchant, buyer } = await openFundedTradeEscrow(reservationId, amount);

      const tradeEscrow = tradeEscrowSeed(reservationId);
      const disputeCase = disputeCasePda(reservationId);

      await withBlockhashRetry(() =>
        program.methods
          .openDisputeCase(new BN(15), new BN(15)) // short windows for the test
          .accountsPartial({
            signer: buyer.publicKey,
            payer: admin.publicKey,
            tradeEscrow,
            disputeCase,
            systemProgram: SystemProgram.programId,
          })
          .signers([buyer])
          .rpc({ commitment: "confirmed" }),
      );

      let escrowAccount = await program.account.tradeEscrowVault.fetch(tradeEscrow);
      expect(escrowAccount.state).to.deep.equal({ frozen: {} });

      // Two arbitrators vote BuyerWins with a combined 90,000 stake;
      // one votes MerchantWins with 60,000 — BuyerWins wins on weighted
      // total, which the tie test below distinguishes from plain vote-count.
      const arb1 = await setUpArbitrator(unit(50000));
      const arb2 = await setUpArbitrator(unit(40000));
      const arb3 = await setUpArbitrator(unit(60000));

      const salt1 = crypto.randomBytes(32);
      const salt2 = crypto.randomBytes(32);
      const salt3 = crypto.randomBytes(32);

      for (const [arb, salt, outcomeByte] of [
        [arb1, salt1, OUTCOME_BYTE.buyerWins],
        [arb2, salt2, OUTCOME_BYTE.buyerWins],
        [arb3, salt3, OUTCOME_BYTE.merchantWins],
      ] as [Keypair, Buffer, number][]) {
        await withBlockhashRetry(() =>
          program.methods
            .commitDisputeVote([...commitmentFor(outcomeByte, salt)])
            .accountsPartial({ arbitrator: arb.publicKey, disputeCase })
            .signers([arb])
            .rpc({ commitment: "confirmed" }),
        );
      }

      await new Promise((r) => setTimeout(r, 16000)); // past commit_deadline

      for (const [arb, salt, outcome] of [
        [arb1, salt1, OUTCOME_BUYER_WINS],
        [arb2, salt2, OUTCOME_BUYER_WINS],
        [arb3, salt3, OUTCOME_MERCHANT_WINS],
      ] as [Keypair, Buffer, any][]) {
        await withBlockhashRetry(() =>
          program.methods
            .revealDisputeVote(outcome, [...salt])
            .accountsPartial({
              arbitrator: arb.publicKey,
              disputeCase,
              arbitratorStake: stakeAccountPda(arb.publicKey),
            })
            .signers([arb])
            .rpc({ commitment: "confirmed" }),
        );
      }

      await new Promise((r) => setTimeout(r, 16000)); // past reveal_deadline

      const liquidityVault = liquidityVaultPda(merchant.publicKey, mint);
      const liquidityTokenVault = liquidityTokenVaultPda(merchant.publicKey, mint);
      const tradeEscrowTokenVault = tradeEscrowTokenVaultPda(reservationId);
      const buyerAta = await ata(mint, buyer.publicKey);

      await withBlockhashRetry(() =>
        program.methods
          .executeDisputeOutcome()
          .accountsPartial({
            mint,
            disputeCase,
            tradeEscrow,
            tradeEscrowTokenVault,
            liquidityVault,
            liquidityTokenVault,
            buyerTokenAccount: buyerAta,
            feeConfig,
            devTreasury,
            ecosystemTreasury,
            infraTreasury,
            emergencyReserve,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
          })
          .rpc({ commitment: "confirmed" }),
      );

      escrowAccount = await program.account.tradeEscrowVault.fetch(tradeEscrow);
      expect(escrowAccount.state).to.deep.equal({ released: {} });

      const feeBaseUnits = amount.mul(new BN(15)).div(new BN(10_000));
      const buyerExpected = amount.sub(feeBaseUnits);
      const buyerTokens = await getAccount(connection, buyerAta, "confirmed", TOKEN_2022_PROGRAM_ID);
      expect(buyerTokens.amount.toString()).to.equal(buyerExpected.toString());

      const caseAccount = await program.account.disputeCase.fetch(disputeCase);
      expect(caseAccount.resolved).to.equal(true);
    });

    it("resolves a weighted tie to InvalidDispute and returns funds to the liquidity vault", async () => {
      const reservationId = 9002;
      const amount = unit(500);
      const { merchant, buyer } = await openFundedTradeEscrow(reservationId, amount);

      const tradeEscrow = tradeEscrowSeed(reservationId);
      const disputeCase = disputeCasePda(reservationId);

      await withBlockhashRetry(() =>
        program.methods
          .openDisputeCase(new BN(15), new BN(15))
          .accountsPartial({
            signer: merchant.publicKey,
            payer: admin.publicKey,
            tradeEscrow,
            disputeCase,
            systemProgram: SystemProgram.programId,
          })
          .signers([merchant])
          .rpc({ commitment: "confirmed" }),
      );

      const arb1 = await setUpArbitrator(unit(50000));
      const arb2 = await setUpArbitrator(unit(50000)); // equal weight, opposing votes

      const salt1 = crypto.randomBytes(32);
      const salt2 = crypto.randomBytes(32);

      for (const [arb, salt, outcomeByte] of [
        [arb1, salt1, OUTCOME_BYTE.buyerWins],
        [arb2, salt2, OUTCOME_BYTE.merchantWins],
      ] as [Keypair, Buffer, number][]) {
        await withBlockhashRetry(() =>
          program.methods
            .commitDisputeVote([...commitmentFor(outcomeByte, salt)])
            .accountsPartial({ arbitrator: arb.publicKey, disputeCase })
            .signers([arb])
            .rpc({ commitment: "confirmed" }),
        );
      }

      await new Promise((r) => setTimeout(r, 16000));

      for (const [arb, salt, outcome] of [
        [arb1, salt1, OUTCOME_BUYER_WINS],
        [arb2, salt2, OUTCOME_MERCHANT_WINS],
      ] as [Keypair, Buffer, any][]) {
        await withBlockhashRetry(() =>
          program.methods
            .revealDisputeVote(outcome, [...salt])
            .accountsPartial({
              arbitrator: arb.publicKey,
              disputeCase,
              arbitratorStake: stakeAccountPda(arb.publicKey),
            })
            .signers([arb])
            .rpc({ commitment: "confirmed" }),
        );
      }

      await new Promise((r) => setTimeout(r, 16000));

      const liquidityVault = liquidityVaultPda(merchant.publicKey, mint);
      const liquidityTokenVault = liquidityTokenVaultPda(merchant.publicKey, mint);
      const tradeEscrowTokenVault = tradeEscrowTokenVaultPda(reservationId);
      const vaultBefore = await program.account.liquidityVault.fetch(liquidityVault);
      // Not actually paid in this outcome (funds return to the liquidity
      // vault instead) — still must be the real buyer's own account,
      // since `buyer_token_account.owner == trade_escrow.buyer` is
      // checked unconditionally regardless of which branch runs.
      const buyerAta = await ata(mint, buyer.publicKey);

      await withBlockhashRetry(() =>
        program.methods
          .executeDisputeOutcome()
          .accountsPartial({
            mint,
            disputeCase,
            tradeEscrow,
            tradeEscrowTokenVault,
            liquidityVault,
            liquidityTokenVault,
            buyerTokenAccount: buyerAta,
            feeConfig,
            devTreasury,
            ecosystemTreasury,
            infraTreasury,
            emergencyReserve,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
          })
          .rpc({ commitment: "confirmed" }),
      );

      const escrowAccount = await program.account.tradeEscrowVault.fetch(tradeEscrow);
      expect(escrowAccount.state).to.deep.equal({ cancelled: {} });

      const vaultAfter = await program.account.liquidityVault.fetch(liquidityVault);
      expect(vaultAfter.available.toString()).to.equal(vaultBefore.available.add(amount).toString());
    });
  });
});
