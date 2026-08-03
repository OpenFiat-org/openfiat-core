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
import {
  getSharedMint,
  getSharedOpenMint,
  getSharedArbitrationPool,
  getSharedFeeConfig,
  getSharedStakingConfig,
  unit,
  MINT_DECIMALS,
} from "./shared-fixtures";

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

  async function mintOpenTokens(dest: PublicKey, amount: BN) {
    const openMintPk = await getSharedOpenMint();
    await mintTo(
      connection,
      admin,
      openMintPk,
      dest,
      admin,
      BigInt(amount.toString()),
      [],
      { commitment: "confirmed" },
      TOKEN_2022_PROGRAM_ID,
    );
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
    // A function rather than a const object. `mint` is assigned in the
    // outer `before` hook, which runs after this block is constructed, so
    // an object literal here would capture `undefined` as the sole
    // allowlisted settlement mint — de-listing the fixture's own mint on
    // every restore and breaking every spec that runs afterwards.
    const original = () => ({
      adListingFee: new BN(0),
      disputeFilingFee: new BN(0),
      // Must match the fixture's own value: `afterEach` restores the shared
      // singleton from this, so a mismatch silently changes the fee every
      // later spec settles at.
      settlementFeeBps: 85,
      devTreasuryBps: 4000,
      ecosystemTreasuryBps: 3000,
      infraTreasuryBps: 2000,
      emergencyReserveBps: 1000,
      timeoutSecs: new BN(1800),
      // Both arbitrator-eligibility gates off, matching what
      // `initialize_fee_config` writes. `afterEach` restores the shared
      // singleton from this object, so leaving them on here would silently
      // gate every dispute spec that runs afterwards — the arbitrators those
      // specs create stake seconds before voting and would all be rejected
      // as too young.
      minArbitratorStakeAgeSecs: new BN(0),
      arbitratorSortitionBps: 0,
      // Restores the allowlist the shared fixture set. Omitting it would
      // leave the singleton allowing nothing this suite can settle in.
      settlementMints: [mint],
    });

    // Restore the shared singleton so later specs see the fixture's own
    // treasuries, whatever this block did to it.
    afterEach(async () => {
      await withBlockhashRetry(() =>
        program.methods
          .updateFeeConfig(original())
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
          .updateFeeConfig({ ...original(), settlementFeeBps: 25 })
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
            .updateFeeConfig(original())
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
            .updateFeeConfig({ ...original(), devTreasuryBps: 4001 })
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
          .updateFeeConfig(original())
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
          .updateFeeConfig(original())
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

      // fee = 1000 * 85bps = 8.5 units -> 8_500_000 base units at 6 decimals
      const feeBaseUnits = amount.mul(new BN(85)).div(new BN(10_000));
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

  // `ad_listing_fee` was stored on FeeConfig from the start and read by no
  // instruction — the same defect the staking minimums had. Advertisements
  // are off-chain gossip records, so the fee is charged against the thing
  // that *is* on-chain: the merchant's OPEN liquidity vault.
  describe("charge_ad_listing_fee", () => {
    // Deliberately three base units above a round OPEN, so 40/30/20/10 of
    // it does *not* divide exactly and the truncation remainder is
    // non-zero. A fee that divides evenly proves the four-way routing but
    // says nothing about where the dust goes, which is the thing OFS-4100
    // §6 actually fixes.
    const LISTING_FEE = unit(1).addn(3);

    it("debits the merchant's OPEN vault and routes the fee to the treasury", async () => {
      const openMint = await getSharedOpenMint();
      const merchant = Keypair.generate();
      await airdrop(merchant.publicKey);

      const openVault = liquidityVaultPda(merchant.publicKey, openMint);
      const openTokenVault = liquidityTokenVaultPda(merchant.publicKey, openMint);
      const openDevTreasury = await ata(openMint, Keypair.generate().publicKey);

      await withBlockhashRetry(() =>
        program.methods
          .createLiquidityVault()
          .accountsPartial({
            merchant: merchant.publicKey,
            mint: openMint,
            liquidityVault: openVault,
            tokenVault: openTokenVault,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
            systemProgram: SystemProgram.programId,
            rent: SYSVAR_RENT_PUBKEY,
          })
          .signers([merchant])
          .rpc({ commitment: "confirmed" }),
      );

      const merchantOpenAta = await ata(openMint, merchant.publicKey);
      await mintOpenTokens(merchantOpenAta, unit(10));
      await withBlockhashRetry(() =>
        program.methods
          .depositLiquidity(unit(10))
          .accountsPartial({
            merchant: merchant.publicKey,
            liquidityVault: openVault,
            tokenVault: openTokenVault,
            from: merchantOpenAta,
            mint: openMint,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
          })
          .signers([merchant])
          .rpc({ commitment: "confirmed" }),
      );

      // Point the shared FeeConfig's treasuries at OPEN-denominated
      // accounts and set a non-zero listing fee.
      const openEco = await ata(openMint, Keypair.generate().publicKey);
      const openInfra = await ata(openMint, Keypair.generate().publicKey);
      const openEmerg = await ata(openMint, Keypair.generate().publicKey);
      await withBlockhashRetry(() =>
        program.methods
          .updateFeeConfig({
            adListingFee: LISTING_FEE,
            disputeFilingFee: new BN(0),
            settlementFeeBps: 85,
            devTreasuryBps: 4000,
            ecosystemTreasuryBps: 3000,
            infraTreasuryBps: 2000,
            emergencyReserveBps: 1000,
            timeoutSecs: new BN(1800),
            minArbitratorStakeAgeSecs: new BN(0),
            arbitratorSortitionBps: 0,
            // The settlement allowlist is carried through unchanged. The
            // ad-listing fee is OPEN-denominated and has nothing to do
            // with which mints may be traded, so repointing the
            // treasuries must not quietly de-list the settlement mint.
            settlementMints: [mint],
          })
          .accountsPartial({
            admin: admin.publicKey,
            feeConfig,
            mint: openMint,
            devTreasury: openDevTreasury,
            ecosystemTreasury: openEco,
            infraTreasury: openInfra,
            emergencyReserve: openEmerg,
          })
          .rpc({ commitment: "confirmed" }),
      );

      const adId = Array.from(crypto.randomBytes(32));
      await withBlockhashRetry(() =>
        program.methods
          .chargeAdListingFee(adId)
          .accountsPartial({
            merchant: merchant.publicKey,
            feeConfig,
            liquidityVault: openVault,
            tokenVault: openTokenVault,
            devTreasury: openDevTreasury,
            ecosystemTreasury: openEco,
            infraTreasury: openInfra,
            emergencyReserve: openEmerg,
            mint: openMint,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
          })
          .signers([merchant])
          .rpc({ commitment: "confirmed" }),
      );

      const remaining = unit(10).sub(LISTING_FEE);
      const vault = await program.account.liquidityVault.fetch(openVault);
      expect(vault.available.toString()).to.equal(remaining.toString());
      expect(vault.total.toString()).to.equal(remaining.toString());

      // Listing fees are protocol revenue and split 40/30/20/10 exactly
      // like the settlement fee, rather than landing wholly in one
      // treasury.
      const balances = await Promise.all(
        [openDevTreasury, openEco, openInfra, openEmerg].map(async (t) =>
          (await getAccount(connection, t, "confirmed", TOKEN_2022_PROGRAM_ID)).amount,
        ),
      );

      // Recomputed from the split rules rather than read back from the
      // program, so a change to one side and not the other is a failure
      // rather than a tautology. The remainder goes to the ECOSYSTEM
      // treasury (index 1) per OFS-4100 §6 — it used to be swept to the
      // emergency reserve, and this assertion is what pins the difference.
      const fee = BigInt(LISTING_FEE.toString());
      const expected = [4000n, 3000n, 2000n, 1000n].map((bps) => (fee * bps) / 10_000n);
      const dust = fee - expected.reduce((sum, v) => sum + v, 0n);
      expect(dust > 0n).to.equal(true, "the fixture fee must not divide evenly");
      expected[1] += dust;

      expect(balances.map(String)).to.deep.equal(expected.map(String));
      expect(
        balances.reduce((sum, v) => sum + v, 0n).toString(),
      ).to.equal(LISTING_FEE.toString());
    });

    it("refuses when the vault cannot cover the fee", async () => {
      const openMint = await getSharedOpenMint();
      const merchant = Keypair.generate();
      await airdrop(merchant.publicKey);
      const openVault = liquidityVaultPda(merchant.publicKey, openMint);
      const openTokenVault = liquidityTokenVaultPda(merchant.publicKey, openMint);
      const liveFeeConfig = await program.account.feeConfig.fetch(feeConfig);
      const openDevTreasury = liveFeeConfig.devTreasury;

      await withBlockhashRetry(() =>
        program.methods
          .createLiquidityVault()
          .accountsPartial({
            merchant: merchant.publicKey,
            mint: openMint,
            liquidityVault: openVault,
            tokenVault: openTokenVault,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
            systemProgram: SystemProgram.programId,
            rent: SYSVAR_RENT_PUBKEY,
          })
          .signers([merchant])
          .rpc({ commitment: "confirmed" }),
      );

      await expectAnchorError(
        program.methods
          .chargeAdListingFee(Array.from(crypto.randomBytes(32)))
          .accountsPartial({
            merchant: merchant.publicKey,
            feeConfig,
            liquidityVault: openVault,
            tokenVault: openTokenVault,
            devTreasury: openDevTreasury,
            ecosystemTreasury: liveFeeConfig.ecosystemTreasury,
            infraTreasury: liveFeeConfig.infraTreasury,
            emergencyReserve: liveFeeConfig.emergencyReserve,
            mint: openMint,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
          })
          .signers([merchant])
          .rpc({ commitment: "confirmed" }),
        "InsufficientAvailableLiquidity",
      );
    });

    // Restore the shared singleton for the specs that follow.
    after(async () => {
      await withBlockhashRetry(() =>
        program.methods
          .updateFeeConfig({
            adListingFee: new BN(0),
            disputeFilingFee: new BN(0),
            settlementFeeBps: 85,
            devTreasuryBps: 4000,
            ecosystemTreasuryBps: 3000,
            infraTreasuryBps: 2000,
            emergencyReserveBps: 1000,
            timeoutSecs: new BN(1800),
            minArbitratorStakeAgeSecs: new BN(0),
            arbitratorSortitionBps: 0,
            settlementMints: [mint],
          })
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
  });

  describe("dispute-to-chain bridge (Phase 4b)", () => {
    const staking = anchor.workspace.staking as Program<Staking>;
    const ROLE_ARBITRATOR = { arbitrator: {} };
    const OUTCOME_BUYER_WINS = { buyerWins: {} };
    const OUTCOME_MERCHANT_WINS = { merchantWins: {} };
    const OUTCOME_MUTUAL_SETTLEMENT = { mutualSettlement: {} };
    const OUTCOME_BYTE = { buyerWins: 0, merchantWins: 1, mutualSettlement: 2, invalidDispute: 3 };

    let stakingConfig: PublicKey;
    let stakeVault: PublicKey;
    let rewardsVault: PublicKey;
    let openMint: PublicKey;
    let arbitrationPool: PublicKey;

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

    /** Basis-points denominator and domain of `shared::sortition`. */
    const SORTITION_BPS_DENOMINATOR = 10_000n;
    const SORTITION_DOMAIN = Buffer.from("openfiat-arbitrator-sortition");

    /**
     * Recomputes a stake account's draw for a case exactly as the program
     * does. Deliberately an independent reimplementation rather than a call
     * into the program: it is what lets the sortition specs pick wallets by
     * their draw and therefore assert a *deterministic* accept and reject,
     * instead of committing from random wallets and hoping the threshold
     * happened to fall the way the test needs.
     */
    function sortitionTicketBps(caseSeed: Buffer, stakeAccount: PublicKey): bigint {
      const digest = crypto
        .createHash("sha256")
        .update(SORTITION_DOMAIN)
        .update(caseSeed)
        .update(stakeAccount.toBuffer())
        .digest();
      return digest.readBigUInt64LE(0) % SORTITION_BPS_DENOMINATOR;
    }

    /** A keypair whose Arbitrator stake account's draw satisfies `predicate`. */
    function findArbitratorByDraw(
      caseSeed: Buffer,
      predicate: (ticket: bigint) => boolean,
    ): Keypair {
      for (let i = 0; i < 20_000; i++) {
        const candidate = Keypair.generate();
        if (predicate(sortitionTicketBps(caseSeed, stakeAccountPda(candidate.publicKey)))) {
          return candidate;
        }
      }
      throw new Error("no keypair matched the requested draw within 20,000 tries");
    }

    async function setUpArbitrator(stakeAmount: BN, existing?: Keypair): Promise<Keypair> {
      const owner = existing ?? Keypair.generate();
      await airdrop(owner.publicKey);
      const stakeAccount = stakeAccountPda(owner.publicKey);

      await withBlockhashRetry(() =>
        staking.methods
          .initializeStakeAccount(ROLE_ARBITRATOR)
          .accountsPartial({ owner: owner.publicKey, stakeAccount, systemProgram: SystemProgram.programId })
          .signers([owner])
          .rpc({ commitment: "confirmed" }),
      );

      // Stake is OPEN, not the mint this trade settles in (OFS-4100 §4).
      // An arbitrator's seat is bought with the protocol token wherever
      // the trade they judge is denominated.
      const ownerAta = await ata(openMint, owner.publicKey);
      await mintOpenTokens(ownerAta, stakeAmount);
      await withBlockhashRetry(() =>
        staking.methods
          .stake(stakeAmount)
          .accountsPartial({
            owner: owner.publicKey,
            stakingConfig,
            stakeAccount,
            stakeVault,
            from: ownerAta,
            mint: openMint,
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
      openVaultFunding: BN = unit(0),
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

      // The merchant also keeps an OPEN vault. That is where the
      // arbitration deposit is taken from when a case opens against them,
      // whoever opened it — see `open_dispute_case`.
      const openVault = liquidityVaultPda(merchant.publicKey, openMint);
      const openTokenVault = liquidityTokenVaultPda(merchant.publicKey, openMint);
      await withBlockhashRetry(() =>
        program.methods
          .createLiquidityVault()
          .accountsPartial({
            merchant: merchant.publicKey,
            mint: openMint,
            liquidityVault: openVault,
            tokenVault: openTokenVault,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
            systemProgram: SystemProgram.programId,
            rent: SYSVAR_RENT_PUBKEY,
          })
          .signers([merchant])
          .rpc({ commitment: "confirmed" }),
      );
      if (!openVaultFunding.isZero()) {
        const merchantOpenAta = await ata(openMint, merchant.publicKey);
        await mintOpenTokens(merchantOpenAta, openVaultFunding);
        await withBlockhashRetry(() =>
          program.methods
            .depositLiquidity(openVaultFunding)
            .accountsPartial({
              merchant: merchant.publicKey,
              liquidityVault: openVault,
              tokenVault: openTokenVault,
              from: merchantOpenAta,
              mint: openMint,
              tokenProgram: TOKEN_2022_PROGRAM_ID,
            })
            .signers([merchant])
            .rpc({ commitment: "confirmed" }),
        );
      }

      return { merchant, buyer, liquidityVault };
    }

    before(async () => {
      ({ stakingConfig, stakeVault, rewardsVault } = await getSharedStakingConfig(staking));
      openMint = await getSharedOpenMint();
      arbitrationPool = await getSharedArbitrationPool(program);
    });

    it("tallies a stake-weighted majority (BuyerWins) and releases funds accordingly", async () => {
      const reservationId = 9001;
      const amount = unit(1000);
      const { merchant, buyer } = await openFundedTradeEscrow(reservationId, amount);

      const tradeEscrow = tradeEscrowSeed(reservationId);
      const disputeCase = disputeCasePda(reservationId);

      await withBlockhashRetry(() =>
        program.methods
          .openDisputeCase(new BN(60), new BN(60)) // the protocol minimum window
          .accountsPartial({
            signer: buyer.publicKey,
            payer: admin.publicKey,
            tradeEscrow,
            disputeCase,
            feeConfig,
            depositMint: openMint,
            merchantOpenVault: liquidityVaultPda(merchant.publicKey, openMint),
            merchantOpenTokenVault: liquidityTokenVaultPda(merchant.publicKey, openMint),
            arbitrationPool,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
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
            .accountsPartial({
              arbitrator: arb.publicKey,
              disputeCase,
              stakingConfig,
              arbitratorStake: stakeAccountPda(arb.publicKey),
            })
            .signers([arb])
            .rpc({ commitment: "confirmed" }),
        );
      }

      await new Promise((r) => setTimeout(r, 61000)); // past commit_deadline

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
              stakingConfig,
              arbitratorStake: stakeAccountPda(arb.publicKey),
            })
            .signers([arb])
            .rpc({ commitment: "confirmed" }),
        );
      }

      await new Promise((r) => setTimeout(r, 61000)); // past reveal_deadline

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
            depositMint: openMint,
            arbitrationPool,
            merchantOpenVault: liquidityVaultPda(merchant.publicKey, openMint),
            merchantOpenTokenVault: liquidityTokenVaultPda(merchant.publicKey, openMint),
            tokenProgram: TOKEN_2022_PROGRAM_ID,
            // The OPEN arbitration deposit moves through its own token
            // program handle, because a mint's owning program is fixed when
            // the mint is created and OPEN need not share one with the
            // settlement stablecoin. Both fixture mints happen to be
            // Token-2022 here, so the two ids coincide — the accounts are
            // still distinct, and on devnet (legacy USDC, Token-2022 OPEN)
            // they diverge.
            depositTokenProgram: TOKEN_2022_PROGRAM_ID,
          })
          .rpc({ commitment: "confirmed" }),
      );

      escrowAccount = await program.account.tradeEscrowVault.fetch(tradeEscrow);
      expect(escrowAccount.state).to.deep.equal({ released: {} });

      const feeBaseUnits = amount.mul(new BN(85)).div(new BN(10_000));
      const buyerExpected = amount.sub(feeBaseUnits);
      const buyerTokens = await getAccount(connection, buyerAta, "confirmed", TOKEN_2022_PROGRAM_ID);
      expect(buyerTokens.amount.toString()).to.equal(buyerExpected.toString());

      const caseAccount = await program.account.disputeCase.fetch(disputeCase);
      expect(caseAccount.resolved).to.equal(true);
    });

    it("re-opens the case on a weighted tie instead of paying either party", async () => {
      const reservationId = 9002;
      const amount = unit(500);
      const { merchant, buyer } = await openFundedTradeEscrow(reservationId, amount);

      const tradeEscrow = tradeEscrowSeed(reservationId);
      const disputeCase = disputeCasePda(reservationId);

      await withBlockhashRetry(() =>
        program.methods
          .openDisputeCase(new BN(60), new BN(60))
          .accountsPartial({
            signer: merchant.publicKey,
            payer: admin.publicKey,
            tradeEscrow,
            disputeCase,
            feeConfig,
            depositMint: openMint,
            merchantOpenVault: liquidityVaultPda(merchant.publicKey, openMint),
            merchantOpenTokenVault: liquidityTokenVaultPda(merchant.publicKey, openMint),
            arbitrationPool,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
            systemProgram: SystemProgram.programId,
          })
          .signers([merchant])
          .rpc({ commitment: "confirmed" }),
      );

      // Three counted votes, so the quorum floor is satisfied and the
      // round is decided purely on the weights: 50,000 each behind two
      // opposing outcomes, with a smaller third vote that cannot break the
      // deadlock. `tally` therefore returns `None` through its tie branch,
      // not through `MIN_ARBITRATORS` — two arbitrators would re-open the
      // case either way, which would leave tie handling untested.
      const arb1 = await setUpArbitrator(unit(50000));
      const arb2 = await setUpArbitrator(unit(50000)); // equal weight, opposing votes
      const arb3 = await setUpArbitrator(unit(20000)); // makes the quorum, loses the vote

      const salt1 = crypto.randomBytes(32);
      const salt2 = crypto.randomBytes(32);
      const salt3 = crypto.randomBytes(32);

      for (const [arb, salt, outcomeByte] of [
        [arb1, salt1, OUTCOME_BYTE.buyerWins],
        [arb2, salt2, OUTCOME_BYTE.merchantWins],
        [arb3, salt3, OUTCOME_BYTE.mutualSettlement],
      ] as [Keypair, Buffer, number][]) {
        await withBlockhashRetry(() =>
          program.methods
            .commitDisputeVote([...commitmentFor(outcomeByte, salt)])
            .accountsPartial({
              arbitrator: arb.publicKey,
              disputeCase,
              stakingConfig,
              arbitratorStake: stakeAccountPda(arb.publicKey),
            })
            .signers([arb])
            .rpc({ commitment: "confirmed" }),
        );
      }

      await new Promise((r) => setTimeout(r, 61000));

      for (const [arb, salt, outcome] of [
        [arb1, salt1, OUTCOME_BUYER_WINS],
        [arb2, salt2, OUTCOME_MERCHANT_WINS],
        [arb3, salt3, OUTCOME_MUTUAL_SETTLEMENT],
      ] as [Keypair, Buffer, any][]) {
        await withBlockhashRetry(() =>
          program.methods
            .revealDisputeVote(outcome, [...salt])
            .accountsPartial({
              arbitrator: arb.publicKey,
              disputeCase,
              stakingConfig,
              arbitratorStake: stakeAccountPda(arb.publicKey),
            })
            .signers([arb])
            .rpc({ commitment: "confirmed" }),
        );
      }

      await new Promise((r) => setTimeout(r, 61000));

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
            depositMint: openMint,
            arbitrationPool,
            merchantOpenVault: liquidityVaultPda(merchant.publicKey, openMint),
            merchantOpenTokenVault: liquidityTokenVaultPda(merchant.publicKey, openMint),
            tokenProgram: TOKEN_2022_PROGRAM_ID,
            depositTokenProgram: TOKEN_2022_PROGRAM_ID,
          })
          .rpc({ commitment: "confirmed" }),
      );

      // A tie is not a verdict. Nothing moves, the escrow stays frozen,
      // and the case re-opens for another round — previously this paid
      // the merchant, which is what made manufacturing a tie worthwhile.
      const escrowAccount = await program.account.tradeEscrowVault.fetch(tradeEscrow);
      expect(escrowAccount.state).to.deep.equal({ frozen: {} });

      const vaultAfter = await program.account.liquidityVault.fetch(liquidityVault);
      expect(vaultAfter.available.toString()).to.equal(vaultBefore.available.toString());

      const caseAccount = await program.account.disputeCase.fetch(disputeCase);
      expect(caseAccount.resolved).to.equal(false);
      expect(caseAccount.round).to.equal(1);
      expect(caseAccount.arbitrators.length).to.equal(0);
      expect(caseAccount.commitments.length).to.equal(0);
      // Both revealed — a tie is a disagreement, not a refusal to speak,
      // so neither seat is retired.
      expect(caseAccount.barred.length, "revealing must never cost a seat").to.equal(0);
    });

    it("bars a seat that committed and never revealed from the rest of the case", async () => {
      // The attack the quorum floor opened. A party who expects to lose
      // takes seats, commits, and stays silent: fewer than three reveals
      // is not a decision, the round re-opens, and doing that until the
      // rounds run out reaches the terminal even split — half an escrow
      // they were going to lose entirely. Re-drawing the seed every round
      // is necessary and not sufficient, because a stake large enough to
      // qualify qualifies again.
      const reservationId = 9013;
      const amount = unit(700);
      const { merchant, buyer } = await openFundedTradeEscrow(reservationId, amount);

      const tradeEscrow = tradeEscrowSeed(reservationId);
      const disputeCase = disputeCasePda(reservationId);

      await withBlockhashRetry(() =>
        program.methods
          .openDisputeCase(new BN(60), new BN(60))
          .accountsPartial({
            signer: buyer.publicKey,
            payer: admin.publicKey,
            tradeEscrow,
            disputeCase,
            feeConfig,
            depositMint: openMint,
            merchantOpenVault: liquidityVaultPda(merchant.publicKey, openMint),
            merchantOpenTokenVault: liquidityTokenVaultPda(merchant.publicKey, openMint),
            arbitrationPool,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
            systemProgram: SystemProgram.programId,
          })
          .signers([buyer])
          .rpc({ commitment: "confirmed" }),
      );

      const honest = await setUpArbitrator(unit(40000));
      const squatter = await setUpArbitrator(unit(90000));
      const buyerAta = await ata(mint, buyer.publicKey);

      const commit = (arb: Keypair, commitment: Buffer) =>
        withBlockhashRetry(() =>
          program.methods
            .commitDisputeVote([...commitment])
            .accountsPartial({
              arbitrator: arb.publicKey,
              disputeCase,
              stakingConfig,
              arbitratorStake: stakeAccountPda(arb.publicKey),
            })
            .signers([arb])
            .rpc({ commitment: "confirmed" }),
        );

      const honestSalt = crypto.randomBytes(32);
      await commit(honest, commitmentFor(OUTCOME_BYTE.buyerWins, honestSalt));
      // The squatter's commitment is never opened. It does not matter what
      // it says — the point is that it is never revealed.
      await commit(squatter, commitmentFor(OUTCOME_BYTE.merchantWins, crypto.randomBytes(32)));

      await new Promise((r) => setTimeout(r, 61000)); // past commit_deadline

      await withBlockhashRetry(() =>
        program.methods
          .revealDisputeVote(OUTCOME_BUYER_WINS, [...honestSalt])
          .accountsPartial({
            arbitrator: honest.publicKey,
            disputeCase,
            stakingConfig,
            arbitratorStake: stakeAccountPda(honest.publicKey),
          })
          .signers([honest])
          .rpc({ commitment: "confirmed" }),
      );

      await new Promise((r) => setTimeout(r, 61000)); // past reveal_deadline
      await withBlockhashRetry(() =>
        program.methods
          .executeDisputeOutcome()
          .accountsPartial({
            mint,
            disputeCase,
            tradeEscrow,
            tradeEscrowTokenVault: tradeEscrowTokenVaultPda(reservationId),
            liquidityVault: liquidityVaultPda(merchant.publicKey, mint),
            liquidityTokenVault: liquidityTokenVaultPda(merchant.publicKey, mint),
            buyerTokenAccount: buyerAta,
            feeConfig,
            devTreasury,
            ecosystemTreasury,
            infraTreasury,
            emergencyReserve,
            depositMint: openMint,
            arbitrationPool,
            merchantOpenVault: liquidityVaultPda(merchant.publicKey, openMint),
            merchantOpenTokenVault: liquidityTokenVaultPda(merchant.publicKey, openMint),
            tokenProgram: TOKEN_2022_PROGRAM_ID,
            depositTokenProgram: TOKEN_2022_PROGRAM_ID,
          })
          .rpc({ commitment: "confirmed" }),
      );

      const reopened = await program.account.disputeCase.fetch(disputeCase);
      expect(reopened.round, "the round must re-open").to.equal(1);
      expect(
        reopened.barred.map((k) => k.toBase58()),
        "the silent seat is barred and the one that revealed is not",
      ).to.deep.equal([squatter.publicKey.toBase58()]);

      // And the bar is enforced, not merely recorded.
      let refused = false;
      try {
        await commit(squatter, commitmentFor(OUTCOME_BYTE.merchantWins, crypto.randomBytes(32)));
      } catch (err) {
        refused = true;
        expect(String(err)).to.match(/ArbitratorBarredFromCase|never revealed/);
      }
      expect(refused, "a barred arbitrator must not take another seat").to.equal(true);

      // The honest arbitrator, who revealed, may serve again.
      await commit(honest, commitmentFor(OUTCOME_BYTE.buyerWins, crypto.randomBytes(32)));
      const round1 = await program.account.disputeCase.fetch(disputeCase);
      expect(round1.arbitrators.map((k) => k.toBase58())).to.deep.equal([
        honest.publicKey.toBase58(),
      ]);
    });

    it("refuses to decide on fewer than three counted votes, however lopsided", async () => {
      const reservationId = 9008;
      const amount = unit(700);
      const { merchant, buyer } = await openFundedTradeEscrow(reservationId, amount);

      const tradeEscrow = tradeEscrowSeed(reservationId);
      const disputeCase = disputeCasePda(reservationId);

      await withBlockhashRetry(() =>
        program.methods
          .openDisputeCase(new BN(60), new BN(60))
          .accountsPartial({
            signer: buyer.publicKey,
            payer: admin.publicKey,
            tradeEscrow,
            disputeCase,
            feeConfig,
            depositMint: openMint,
            merchantOpenVault: liquidityVaultPda(merchant.publicKey, openMint),
            merchantOpenTokenVault: liquidityTokenVaultPda(merchant.publicKey, openMint),
            arbitrationPool,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
            systemProgram: SystemProgram.programId,
          })
          .signers([buyer])
          .rpc({ commitment: "confirmed" }),
      );

      // All three are staked up front: a re-opened round only allows the
      // windows it was created with, so there is no time to stake inside
      // one.
      const arb1 = await setUpArbitrator(unit(70000));
      const arb2 = await setUpArbitrator(unit(20000));
      const arb3 = await setUpArbitrator(unit(30000));

      const liquidityVault = liquidityVaultPda(merchant.publicKey, mint);
      const liquidityTokenVault = liquidityTokenVaultPda(merchant.publicKey, mint);
      const tradeEscrowTokenVault = tradeEscrowTokenVaultPda(reservationId);
      const buyerAta = await ata(mint, buyer.publicKey);

      const execute = () =>
        withBlockhashRetry(() =>
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
              depositMint: openMint,
              arbitrationPool,
              merchantOpenVault: liquidityVaultPda(merchant.publicKey, openMint),
              merchantOpenTokenVault: liquidityTokenVaultPda(merchant.publicKey, openMint),
              tokenProgram: TOKEN_2022_PROGRAM_ID,
              depositTokenProgram: TOKEN_2022_PROGRAM_ID,
            })
            .rpc({ commitment: "confirmed" }),
        );

      // One full round: everyone named votes the same way, then the round
      // is executed once its reveal window has closed.
      async function voteRound(voters: Keypair[]) {
        const salts = voters.map(() => crypto.randomBytes(32));
        for (const [i, arb] of voters.entries()) {
          await withBlockhashRetry(() =>
            program.methods
              .commitDisputeVote([...commitmentFor(OUTCOME_BYTE.buyerWins, salts[i])])
              .accountsPartial({
                arbitrator: arb.publicKey,
                disputeCase,
                stakingConfig,
                arbitratorStake: stakeAccountPda(arb.publicKey),
              })
              .signers([arb])
              .rpc({ commitment: "confirmed" }),
          );
        }

        await new Promise((r) => setTimeout(r, 61000)); // past commit_deadline

        for (const [i, arb] of voters.entries()) {
          await withBlockhashRetry(() =>
            program.methods
              .revealDisputeVote(OUTCOME_BUYER_WINS, [...salts[i]])
              .accountsPartial({
                arbitrator: arb.publicKey,
                disputeCase,
                stakingConfig,
                arbitratorStake: stakeAccountPda(arb.publicKey),
              })
              .signers([arb])
              .rpc({ commitment: "confirmed" }),
          );
        }

        await new Promise((r) => setTimeout(r, 61000)); // past reveal_deadline
        await execute();
      }

      const buyerBefore = await getAccount(connection, buyerAta, "confirmed", TOKEN_2022_PROGRAM_ID);
      const vaultBefore = await program.account.liquidityVault.fetch(liquidityVault);
      // Captured so the re-opened round's seed can be compared against it.
      const openedCase = await program.account.disputeCase.fetch(disputeCase);

      // Two votes, 70,000 against 20,000 — unanimous, so nothing about
      // this round is close and nothing is tied. It still decides nothing,
      // because the floor is on how many arbitrators showed up and not on
      // how much stake they brought. One arbitrator settling a dispute
      // alone was exactly the bug.
      await voteRound([arb1, arb2]);

      const midCase = await program.account.disputeCase.fetch(disputeCase);
      expect(midCase.resolved, "two arbitrators must not be able to decide a case").to.equal(false);
      expect(midCase.round, "the case must re-open for another round").to.equal(1);
      expect(midCase.outcome ?? null, "no verdict may be recorded").to.equal(null);

      // The re-opened round must draw a fresh sortition seed. Carrying the
      // old one over would mean the same wallets qualify every round, so an
      // attacker who won the draw once would hold those seats for the rest
      // of the case — and forcing a re-round is something they can do
      // deliberately by committing and never revealing.
      expect(
        Buffer.from(midCase.caseSeed).equals(Buffer.from(openedCase.caseSeed)),
        "a re-opened round must re-draw its seed",
      ).to.equal(false);
      expect(
        midCase.roundOpenedAt.gt(openedCase.roundOpenedAt),
        "the new round's draw window must start when the round did",
      ).to.equal(true);

      const midEscrow = await program.account.tradeEscrowVault.fetch(tradeEscrow);
      expect(midEscrow.state).to.deep.equal({ frozen: {} });

      const buyerMid = await getAccount(connection, buyerAta, "confirmed", TOKEN_2022_PROGRAM_ID);
      expect(buyerMid.amount.toString(), "the buyer must not be paid").to.equal(
        buyerBefore.amount.toString(),
      );
      const vaultMid = await program.account.liquidityVault.fetch(liquidityVault);
      expect(vaultMid.available.toString(), "the merchant must not be paid").to.equal(
        vaultBefore.available.toString(),
      );

      // A quorum requirement, not a permanent block: the same votes plus a
      // third arbitrator now carry the same outcome.
      await voteRound([arb1, arb2, arb3]);

      const finalCase = await program.account.disputeCase.fetch(disputeCase);
      expect(finalCase.resolved).to.equal(true);
      expect(finalCase.outcome).to.deep.equal(OUTCOME_BUYER_WINS);

      const finalEscrow = await program.account.tradeEscrowVault.fetch(tradeEscrow);
      expect(finalEscrow.state).to.deep.equal({ released: {} });

      const feeBaseUnits = amount.mul(new BN(85)).div(new BN(10_000));
      const buyerAfter = await getAccount(connection, buyerAta, "confirmed", TOKEN_2022_PROGRAM_ID);
      expect((buyerAfter.amount - buyerBefore.amount).toString()).to.equal(
        amount.sub(feeBaseUnits).toString(),
      );
    });

    it("rejects a commit from an arbitrator below the role minimum, blocking the seat-squatting attack", async () => {
      const reservationId = 9003;
      const amount = unit(400);
      const { merchant, buyer } = await openFundedTradeEscrow(reservationId, amount);

      const tradeEscrow = tradeEscrowSeed(reservationId);
      const disputeCase = disputeCasePda(reservationId);

      await withBlockhashRetry(() =>
        program.methods
          .openDisputeCase(new BN(60), new BN(60))
          .accountsPartial({
            signer: buyer.publicKey,
            payer: admin.publicKey,
            tradeEscrow,
            disputeCase,
            feeConfig,
            depositMint: openMint,
            merchantOpenVault: liquidityVaultPda(merchant.publicKey, openMint),
            merchantOpenTokenVault: liquidityTokenVaultPda(merchant.publicKey, openMint),
            arbitrationPool,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
            systemProgram: SystemProgram.programId,
          })
          .signers([buyer])
          .rpc({ commitment: "confirmed" }),
      );

      // The attack: a wallet with an initialized-but-empty Arbitrator
      // stake account. `initialize_stake_account` is permissionless and a
      // zero balance is legal, so seven of these could once occupy every
      // seat for the price of rent and force the tally to a tie.
      const squatter = Keypair.generate();
      await airdrop(squatter.publicKey);
      const squatterStake = stakeAccountPda(squatter.publicKey);
      await withBlockhashRetry(() =>
        staking.methods
          .initializeStakeAccount(ROLE_ARBITRATOR)
          .accountsPartial({
            owner: squatter.publicKey,
            stakeAccount: squatterStake,
            systemProgram: SystemProgram.programId,
          })
          .signers([squatter])
          .rpc({ commitment: "confirmed" }),
      );

      const zeroStake = await staking.account.stakeAccount.fetch(squatterStake);
      expect(zeroStake.amount.toString()).to.equal("0");

      let rejected = false;
      try {
        await program.methods
          .commitDisputeVote([...commitmentFor(OUTCOME_BYTE.merchantWins, crypto.randomBytes(32))])
          .accountsPartial({
            arbitrator: squatter.publicKey,
            disputeCase,
            stakingConfig,
            arbitratorStake: squatterStake,
          })
          .signers([squatter])
          .rpc({ commitment: "confirmed" });
      } catch (err: any) {
        rejected = true;
        expect(err.toString()).to.contain("ArbitratorStakeBelowMinimum");
      }
      expect(rejected, "a zero-stake wallet must not be able to occupy a seat").to.equal(true);

      // And the seat really is still free.
      const caseAccount = await program.account.disputeCase.fetch(disputeCase);
      expect(caseAccount.arbitrators.length).to.equal(0);
    });

    it("rejects a partially-staked arbitrator, not just a zero-balance one", async () => {
      const reservationId = 9004;
      const amount = unit(400);
      const { merchant, buyer } = await openFundedTradeEscrow(reservationId, amount);

      const tradeEscrow = tradeEscrowSeed(reservationId);
      const disputeCase = disputeCasePda(reservationId);

      await withBlockhashRetry(() =>
        program.methods
          .openDisputeCase(new BN(60), new BN(60))
          .accountsPartial({
            signer: buyer.publicKey,
            payer: admin.publicKey,
            tradeEscrow,
            disputeCase,
            feeConfig,
            depositMint: openMint,
            merchantOpenVault: liquidityVaultPda(merchant.publicKey, openMint),
            merchantOpenTokenVault: liquidityTokenVaultPda(merchant.publicKey, openMint),
            arbitrationPool,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
            systemProgram: SystemProgram.programId,
          })
          .signers([buyer])
          .rpc({ commitment: "confirmed" }),
      );

      // The staking program only permits a balance of zero or >= the
      // minimum, so "some but not enough" cannot be reached by staking.
      // Requesting an unstake down to zero is the reachable way to stop
      // qualifying while an account still exists.
      const arb = await setUpArbitrator(unit(10000));
      const arbStake = stakeAccountPda(arb.publicKey);
      await withBlockhashRetry(() =>
        staking.methods
          .requestUnstake(unit(10000))
          .accountsPartial({ owner: arb.publicKey, stakingConfig, stakeAccount: arbStake })
          .signers([arb])
          .rpc({ commitment: "confirmed" }),
      );

      const drained = await staking.account.stakeAccount.fetch(arbStake);
      expect(drained.amount.toString()).to.equal("0");

      let rejected = false;
      try {
        await program.methods
          .commitDisputeVote([...commitmentFor(OUTCOME_BYTE.buyerWins, crypto.randomBytes(32))])
          .accountsPartial({
            arbitrator: arb.publicKey,
            disputeCase,
            stakingConfig,
            arbitratorStake: arbStake,
          })
          .signers([arb])
          .rpc({ commitment: "confirmed" });
      } catch (err: any) {
        rejected = true;
        expect(err.toString()).to.contain("ArbitratorStakeBelowMinimum");
      }
      expect(rejected, "unbonding out of the minimum must forfeit the right to commit").to.equal(
        true,
      );
    });

    it("rejects dispute windows outside the permitted range", async () => {
      const reservationId = 9005;
      const amount = unit(300);
      const { merchant, buyer } = await openFundedTradeEscrow(reservationId, amount);

      const tradeEscrow = tradeEscrowSeed(reservationId);
      const disputeCase = disputeCasePda(reservationId);

      // A one-second commit window closes before any honest arbitrator
      // could see the case — the opener is a party to the trade.
      for (const [commitWindow, revealWindow] of [
        [new BN(1), new BN(60)],
        [new BN(60), new BN(1)],
        [new BN(60 * 60 * 24 * 30), new BN(60)],
      ] as [BN, BN][]) {
        let rejected = false;
        try {
          await program.methods
            .openDisputeCase(commitWindow, revealWindow)
            .accountsPartial({
              signer: buyer.publicKey,
              payer: admin.publicKey,
              tradeEscrow,
              disputeCase,
              feeConfig,
              depositMint: openMint,
              merchantOpenVault: liquidityVaultPda(merchant.publicKey, openMint),
              merchantOpenTokenVault: liquidityTokenVaultPda(merchant.publicKey, openMint),
              arbitrationPool,
              tokenProgram: TOKEN_2022_PROGRAM_ID,
              systemProgram: SystemProgram.programId,
            })
            .signers([buyer])
            .rpc({ commitment: "confirmed" });
        } catch (err: any) {
          rejected = true;
          expect(err.toString()).to.contain("DisputeWindowOutOfRange");
        }
        expect(
          rejected,
          `window pair ${commitWindow.toString()}/${revealWindow.toString()} must be refused`,
        ).to.equal(true);
      }
    });

    it("splits the escrow evenly on a MutualSettlement verdict rather than paying the merchant", async () => {
      const reservationId = 9006;
      const amount = unit(500);
      const { merchant, buyer } = await openFundedTradeEscrow(reservationId, amount);

      const tradeEscrow = tradeEscrowSeed(reservationId);
      const disputeCase = disputeCasePda(reservationId);

      await withBlockhashRetry(() =>
        program.methods
          .openDisputeCase(new BN(60), new BN(60))
          .accountsPartial({
            signer: buyer.publicKey,
            payer: admin.publicKey,
            tradeEscrow,
            disputeCase,
            feeConfig,
            depositMint: openMint,
            merchantOpenVault: liquidityVaultPda(merchant.publicKey, openMint),
            merchantOpenTokenVault: liquidityTokenVaultPda(merchant.publicKey, openMint),
            arbitrationPool,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
            systemProgram: SystemProgram.programId,
          })
          .signers([buyer])
          .rpc({ commitment: "confirmed" }),
      );

      // Three unanimous arbitrators, because a MutualSettlement *verdict*
      // is what this test is about: fewer than `MIN_ARBITRATORS` counted
      // votes decides nothing at all, so a single arbitrator here would
      // silently turn this into another quorum-floor test.
      const arbs = [
        await setUpArbitrator(unit(50000)),
        await setUpArbitrator(unit(40000)),
        await setUpArbitrator(unit(30000)),
      ];
      const salts = arbs.map(() => crypto.randomBytes(32));

      for (const [arb, salt] of arbs.map((a, i) => [a, salts[i]] as [Keypair, Buffer])) {
        await withBlockhashRetry(() =>
          program.methods
            .commitDisputeVote([...commitmentFor(OUTCOME_BYTE.mutualSettlement, salt)])
            .accountsPartial({
              arbitrator: arb.publicKey,
              disputeCase,
              stakingConfig,
              arbitratorStake: stakeAccountPda(arb.publicKey),
            })
            .signers([arb])
            .rpc({ commitment: "confirmed" }),
        );
      }

      await new Promise((r) => setTimeout(r, 61000));

      for (const [arb, salt] of arbs.map((a, i) => [a, salts[i]] as [Keypair, Buffer])) {
        await withBlockhashRetry(() =>
          program.methods
            .revealDisputeVote(OUTCOME_MUTUAL_SETTLEMENT, [...salt])
            .accountsPartial({
              arbitrator: arb.publicKey,
              disputeCase,
              stakingConfig,
              arbitratorStake: stakeAccountPda(arb.publicKey),
            })
            .signers([arb])
            .rpc({ commitment: "confirmed" }),
        );
      }

      await new Promise((r) => setTimeout(r, 61000));

      const liquidityVault = liquidityVaultPda(merchant.publicKey, mint);
      const liquidityTokenVault = liquidityTokenVaultPda(merchant.publicKey, mint);
      const tradeEscrowTokenVault = tradeEscrowTokenVaultPda(reservationId);
      const buyerAta = await ata(mint, buyer.publicKey);
      const buyerBefore = await getAccount(connection, buyerAta, "confirmed", TOKEN_2022_PROGRAM_ID);
      const vaultBefore = await program.account.liquidityVault.fetch(liquidityVault);

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
            depositMint: openMint,
            arbitrationPool,
            merchantOpenVault: liquidityVaultPda(merchant.publicKey, openMint),
            merchantOpenTokenVault: liquidityTokenVaultPda(merchant.publicKey, openMint),
            tokenProgram: TOKEN_2022_PROGRAM_ID,
            depositTokenProgram: TOKEN_2022_PROGRAM_ID,
          })
          .rpc({ commitment: "confirmed" }),
      );

      const half = amount.div(new BN(2));
      const buyerAfter = await getAccount(connection, buyerAta, "confirmed", TOKEN_2022_PROGRAM_ID);
      expect((buyerAfter.amount - buyerBefore.amount).toString()).to.equal(half.toString());

      const vaultAfter = await program.account.liquidityVault.fetch(liquidityVault);
      expect(vaultAfter.available.toString()).to.equal(
        vaultBefore.available.add(amount.sub(half)).toString(),
      );

      const escrowAccount = await program.account.tradeEscrowVault.fetch(tradeEscrow);
      expect(escrowAccount.state).to.deep.equal({ released: {} });
    });

    it("splits evenly once the round limit is exhausted, so stalling wins nobody the escrow", async () => {
      const reservationId = 9007;
      const amount = unit(600);
      const { merchant, buyer } = await openFundedTradeEscrow(reservationId, amount);

      const tradeEscrow = tradeEscrowSeed(reservationId);
      const disputeCase = disputeCasePda(reservationId);

      await withBlockhashRetry(() =>
        program.methods
          .openDisputeCase(new BN(60), new BN(60))
          .accountsPartial({
            signer: buyer.publicKey,
            payer: admin.publicKey,
            tradeEscrow,
            disputeCase,
            feeConfig,
            depositMint: openMint,
            merchantOpenVault: liquidityVaultPda(merchant.publicKey, openMint),
            merchantOpenTokenVault: liquidityTokenVaultPda(merchant.publicKey, openMint),
            arbitrationPool,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
            systemProgram: SystemProgram.programId,
          })
          .signers([buyer])
          .rpc({ commitment: "confirmed" }),
      );

      const liquidityVault = liquidityVaultPda(merchant.publicKey, mint);
      const liquidityTokenVault = liquidityTokenVaultPda(merchant.publicKey, mint);
      const tradeEscrowTokenVault = tradeEscrowTokenVaultPda(reservationId);
      const buyerAta = await ata(mint, buyer.publicKey);
      const buyerBefore = await getAccount(connection, buyerAta, "confirmed", TOKEN_2022_PROGRAM_ID);
      const vaultBefore = await program.account.liquidityVault.fetch(liquidityVault);

      const execute = () =>
        withBlockhashRetry(() =>
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
              depositMint: openMint,
              arbitrationPool,
              merchantOpenVault: liquidityVaultPda(merchant.publicKey, openMint),
              merchantOpenTokenVault: liquidityTokenVaultPda(merchant.publicKey, openMint),
              tokenProgram: TOKEN_2022_PROGRAM_ID,
              depositTokenProgram: TOKEN_2022_PROGRAM_ID,
            })
            .rpc({ commitment: "confirmed" }),
        );

      // Nobody ever reveals. Each expiry re-opens the case rather than
      // paying out, until the bound is hit — at which point the escrow
      // must not be left frozen forever either.
      for (let round = 0; round < 2; round++) {
        await new Promise((r) => setTimeout(r, 121000)); // past both windows
        await execute();
        const mid = await program.account.disputeCase.fetch(disputeCase);
        expect(mid.resolved).to.equal(false);
        expect(mid.round).to.equal(round + 1);
        const escrowMid = await program.account.tradeEscrowVault.fetch(tradeEscrow);
        expect(escrowMid.state).to.deep.equal({ frozen: {} });
      }

      await new Promise((r) => setTimeout(r, 121000));
      await execute();

      const half = amount.div(new BN(2));
      const buyerAfter = await getAccount(connection, buyerAta, "confirmed", TOKEN_2022_PROGRAM_ID);
      expect((buyerAfter.amount - buyerBefore.amount).toString()).to.equal(half.toString());

      const vaultAfter = await program.account.liquidityVault.fetch(liquidityVault);
      expect(vaultAfter.available.toString()).to.equal(
        vaultBefore.available.add(amount.sub(half)).toString(),
      );

      const caseAccount = await program.account.disputeCase.fetch(disputeCase);
      expect(caseAccount.resolved).to.equal(true);
    });

    // --- arbitrator eligibility: stake age and per-case sortition ---------
    //
    // Both gates are governance parameters that ship DISABLED, so every spec
    // above runs with them off. These turn them on deliberately and restore
    // them afterwards, because the FeeConfig is a shared singleton.
    describe("arbitrator eligibility gates (OFS-4100 §4, §4.1)", () => {
      /** Rewrites just the two eligibility gates, leaving the fees alone. */
      async function setGates(minAgeSecs: number, sortitionBps: number) {
        await withBlockhashRetry(() =>
          program.methods
            .updateFeeConfig({
              adListingFee: new BN(0),
              disputeFilingFee: new BN(0),
              settlementFeeBps: 85,
              devTreasuryBps: 4000,
              ecosystemTreasuryBps: 3000,
              infraTreasuryBps: 2000,
              emergencyReserveBps: 1000,
              timeoutSecs: new BN(1800),
              minArbitratorStakeAgeSecs: new BN(minAgeSecs),
              arbitratorSortitionBps: sortitionBps,
              settlementMints: [mint],
            })
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
      }

      // Restore the disabled state whatever a spec in here did, so the
      // singleton is not left gating anything that runs later.
      afterEach(async () => {
        await setGates(0, 0);
      });

      function commitAs(arb: Keypair, disputeCase: PublicKey, outcomeByte: number, salt: Buffer) {
        return withBlockhashRetry(() =>
          program.methods
            .commitDisputeVote([...commitmentFor(outcomeByte, salt)])
            .accountsPartial({
              arbitrator: arb.publicKey,
              disputeCase,
              stakingConfig,
              arbitratorStake: stakeAccountPda(arb.publicKey),
              feeConfig,
            })
            .signers([arb])
            .rpc({ commitment: "confirmed" }),
        );
      }

      /** Opens a case with the given commit window and returns its seed. */
      async function openCase(
        reservationId: number,
        commitWindowSecs: number,
      ): Promise<{ disputeCase: PublicKey; caseSeed: Buffer }> {
        const amount = unit(300);
        const { merchant, buyer } = await openFundedTradeEscrow(reservationId, amount);
        const disputeCase = disputeCasePda(reservationId);
        await withBlockhashRetry(() =>
          program.methods
            .openDisputeCase(new BN(commitWindowSecs), new BN(60))
            .accountsPartial({
              signer: buyer.publicKey,
              payer: admin.publicKey,
              tradeEscrow: tradeEscrowSeed(reservationId),
              disputeCase,
              feeConfig,
              depositMint: openMint,
              merchantOpenVault: liquidityVaultPda(merchant.publicKey, openMint),
              merchantOpenTokenVault: liquidityTokenVaultPda(merchant.publicKey, openMint),
              arbitrationPool,
              tokenProgram: TOKEN_2022_PROGRAM_ID,
              systemProgram: SystemProgram.programId,
            })
            .signers([buyer])
            .rpc({ commitment: "confirmed" }),
        );
        const account = await program.account.disputeCase.fetch(disputeCase);
        return { disputeCase, caseSeed: Buffer.from(account.caseSeed) };
      }

      it("seeds every case from a slot hash, so two cases never share a draw", async () => {
        const first = await openCase(9101, 600);
        const second = await openCase(9102, 600);

        // A seed of all zeros would mean the sysvar read silently produced
        // nothing and every wallet's draw became a constant.
        expect(first.caseSeed.equals(Buffer.alloc(32)), "seed must not be zero").to.equal(false);
        expect(
          first.caseSeed.equals(second.caseSeed),
          "two cases must not share a seed, or one ground draw would win both",
        ).to.equal(false);
      });

      it("refuses a commit from an arbitrator whose stake is younger than the configured age", async () => {
        const { disputeCase } = await openCase(9103, 600);
        // Staked seconds ago, so any positive requirement excludes it.
        const arb = await setUpArbitrator(unit(50000));
        await setGates(3600, 0);

        await expectAnchorError(
          commitAs(arb, disputeCase, OUTCOME_BYTE.buyerWins, crypto.randomBytes(32)),
          "ArbitratorStakeTooYoung",
        );

        // The same wallet, unchanged, once the requirement is lifted — so the
        // rejection was the age gate and not something else about the wallet.
        await setGates(0, 0);
        await commitAs(arb, disputeCase, OUTCOME_BYTE.buyerWins, crypto.randomBytes(32));
        const account = await program.account.disputeCase.fetch(disputeCase);
        expect(account.arbitrators.map((a: PublicKey) => a.toBase58())).to.include(
          arb.publicKey.toBase58(),
        );
      });

      it("admits a drawn wallet and refuses an undrawn one at the same threshold", async () => {
        // A 7-day commit window keeps the threshold in its first, tightest
        // slice for the whole spec. With a 60-second window the widening
        // would move under the test's own feet.
        const { disputeCase, caseSeed } = await openCase(9104, 7 * 24 * 60 * 60);
        const THRESHOLD = 100;

        const drawn = findArbitratorByDraw(caseSeed, (t) => t < BigInt(THRESHOLD));
        const undrawn = findArbitratorByDraw(caseSeed, (t) => t >= BigInt(THRESHOLD));
        await setUpArbitrator(unit(50000), drawn);
        await setUpArbitrator(unit(50000), undrawn);
        await setGates(0, THRESHOLD);

        // Identical stake, identical role, identical case. The only thing
        // separating them is the draw.
        await commitAs(drawn, disputeCase, OUTCOME_BYTE.buyerWins, crypto.randomBytes(32));
        await expectAnchorError(
          commitAs(undrawn, disputeCase, OUTCOME_BYTE.buyerWins, crypto.randomBytes(32)),
          "NotDrawnForThisCase",
        );

        const account = await program.account.disputeCase.fetch(disputeCase);
        const seated = account.arbitrators.map((a: PublicKey) => a.toBase58());
        expect(seated).to.include(drawn.publicKey.toBase58());
        expect(seated).to.not.include(undrawn.publicKey.toBase58());
      });

      it("widens the draw across the window, so a thin pool can still fill a case", async () => {
        // The liveness half of sortition. Without widening, a pool smaller
        // than roughly `seats / threshold` could never fill a case and every
        // dispute would end in the terminal even split.
        const COMMIT_WINDOW = 80;
        const { disputeCase, caseSeed } = await openCase(9105, COMMIT_WINDOW);
        // Excluded at every threshold the schedule passes through before the
        // final slice, so the only thing that can admit it is the widening.
        const undrawn = findArbitratorByDraw(caseSeed, (t) => t >= 6_400n);
        await setUpArbitrator(unit(50000), undrawn);
        await setGates(0, 100);

        await expectAnchorError(
          commitAs(undrawn, disputeCase, OUTCOME_BYTE.buyerWins, crypto.randomBytes(32)),
          "NotDrawnForThisCase",
        );

        // Into the final eighth of the window, where the gate is open to
        // everyone regardless of their draw.
        await new Promise((r) => setTimeout(r, (COMMIT_WINDOW - 6) * 1000));
        await commitAs(undrawn, disputeCase, OUTCOME_BYTE.buyerWins, crypto.randomBytes(32));

        const account = await program.account.disputeCase.fetch(disputeCase);
        expect(account.arbitrators.map((a: PublicKey) => a.toBase58())).to.include(
          undrawn.publicKey.toBase58(),
        );
      });
    });
  });
});
