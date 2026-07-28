import * as anchor from "@anchor-lang/core";
import { Program, BN } from "@anchor-lang/core";
import { Escrow } from "../target/types/escrow";
import {
  TOKEN_2022_PROGRAM_ID,
  createMint,
  mintTo,
  getOrCreateAssociatedTokenAccount,
  getAccount,
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

describe("escrow", () => {
  anchor.setProvider(anchor.AnchorProvider.env());
  const provider = anchor.AnchorProvider.env();
  const connection = provider.connection;

  const program = anchor.workspace.escrow as Program<Escrow>;
  const admin = (provider.wallet as anchor.Wallet).payer;

  const MINT_DECIMALS = 6;
  const unit = (n: number) => new BN(n).mul(new BN(10).pow(new BN(MINT_DECIMALS)));

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
  function feeConfigPda() {
    return PublicKey.findProgramAddressSync([Buffer.from("fee_config")], program.programId)[0];
  }

  before(async () => {
    mint = await createMint(
      connection,
      admin,
      admin.publicKey,
      null,
      MINT_DECIMALS,
      undefined,
      { commitment: "confirmed" },
      TOKEN_2022_PROGRAM_ID,
    );

    // Four distinct owner wallets — `ata(mint, owner)` is deterministic per
    // (mint, owner), so reusing one owner four times would collapse all
    // four treasuries onto the same token account.
    devTreasury = await ata(mint, Keypair.generate().publicKey);
    ecosystemTreasury = await ata(mint, Keypair.generate().publicKey);
    infraTreasury = await ata(mint, Keypair.generate().publicKey);
    emergencyReserve = await ata(mint, Keypair.generate().publicKey);

    feeConfig = feeConfigPda();
    await withBlockhashRetry(() =>
        program.methods
      .initializeFeeConfig({
        adListingFee: new BN(0),
        disputeFilingFee: new BN(0),
        settlementFeeBps: 15, // 0.15%, matching openfiat-app's OFIP-0021 mock
        devTreasury,
        ecosystemTreasury,
        infraTreasury,
        emergencyReserve,
        devTreasuryBps: 4000,
        ecosystemTreasuryBps: 3000,
        infraTreasuryBps: 2000,
        emergencyReserveBps: 1000,
        timeoutSecs: new BN(1800),
      })
      .accountsPartial({
        admin: admin.publicKey,
        feeConfig,
        systemProgram: SystemProgram.programId,
      })
      .rpc({ commitment: "confirmed" }),
      );
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
        .createTradeEscrow(new BN(reservationId), amount, merchant.publicKey, new BN(1800))
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
        .createTradeEscrow(new BN(reservationId), amount, merchant.publicKey, new BN(1))
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
});
