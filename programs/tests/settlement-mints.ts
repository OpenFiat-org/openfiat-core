// Legacy-SPL settlement, and the settlement-mint allowlist.
//
// Two claims live here, and the first one is the reason the file exists.
//
// 1. The escrow can hold a mint owned by the LEGACY SPL Token program.
//    Every other spec in this suite settles in a Token-2022 fixture mint,
//    and did so while the programs declared `Program<'info, Token2022>` —
//    a constraint under which wSOL, USDC and USDT could not be escrowed at
//    all, because all three are legacy-SPL on devnet and mainnet. The
//    fixtures had been written to match the constraint rather than to match
//    production, so the entire suite passed while the protocol could not
//    hold a single asset it exists to hold. A green run of the OTHER specs
//    proves nothing about this, which is why the legacy cycle below is run
//    end to end rather than asserted at the account level.
//
// 2. A mint that is not on `FeeConfig`'s allowlist cannot be escrowed, and
//    de-listing one strands nothing that is already deposited.
import * as anchor from "@anchor-lang/core";
import { Program, BN } from "@anchor-lang/core";
import { Escrow } from "../target/types/escrow";
import {
  TOKEN_PROGRAM_ID,
  TOKEN_2022_PROGRAM_ID,
  createMint,
  getAccount,
  getOrCreateAssociatedTokenAccount,
  mintTo,
} from "@solana/spl-token";
import {
  Keypair,
  PublicKey,
  SystemProgram,
  SYSVAR_RENT_PUBKEY,
} from "@solana/web3.js";
import { expect } from "chai";
import {
  MINT_DECIMALS,
  SHARED_FEE_PARAMS,
  getSharedFeeConfig,
  getSharedMint,
  unit,
} from "./shared-fixtures";

describe("settlement mints: legacy SPL support and the allowlist", () => {
  anchor.setProvider(anchor.AnchorProvider.env());
  const provider = anchor.AnchorProvider.env();
  const connection = provider.connection;

  const program = anchor.workspace.escrow as Program<Escrow>;
  const admin = (provider.wallet as anchor.Wallet).payer;

  let sharedMint: PublicKey;
  let feeConfig: PublicKey;
  let devTreasury: PublicKey;
  let ecosystemTreasury: PublicKey;
  let infraTreasury: PublicKey;
  let emergencyReserve: PublicKey;

  /**
   * A mint owned by the LEGACY SPL Token program — the thing this file is
   * about. `TOKEN_PROGRAM_ID`, not `TOKEN_2022_PROGRAM_ID`.
   */
  let legacyMint: PublicKey;
  let legacyTreasuries: {
    dev: PublicKey;
    eco: PublicKey;
    infra: PublicKey;
    emerg: PublicKey;
  };

  async function airdrop(pubkey: PublicKey, sol = 10) {
    const sig = await connection.requestAirdrop(pubkey, sol * 1_000_000_000);
    const latest = await connection.getLatestBlockhash();
    await connection.confirmTransaction({ signature: sig, ...latest });
  }

  async function ata(
    mintPk: PublicKey,
    owner: PublicKey,
    tokenProgram: PublicKey,
  ): Promise<PublicKey> {
    const acc = await getOrCreateAssociatedTokenAccount(
      connection,
      admin,
      mintPk,
      owner,
      false,
      "confirmed",
      { commitment: "confirmed" },
      tokenProgram,
    );
    return acc.address;
  }

  async function mintTokens(
    mintPk: PublicKey,
    dest: PublicKey,
    amount: BN,
    tokenProgram: PublicKey,
  ) {
    await mintTo(
      connection,
      admin,
      mintPk,
      dest,
      admin,
      BigInt(amount.toString()),
      [],
      { commitment: "confirmed" },
      tokenProgram,
    );
  }

  async function withBlockhashRetry<T>(fn: () => Promise<T>, attempts = 4): Promise<T> {
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
      [
        Buffer.from("liquidity_vault_tokens"),
        merchant.toBuffer(),
        mintPk.toBuffer(),
      ],
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
      [
        Buffer.from("trade_escrow_tokens"),
        new BN(reservationId).toArrayLike(Buffer, "le", 8),
      ],
      program.programId,
    )[0];
  }

  /**
   * Rewrites the shared singleton's allowlist and treasuries in one call.
   *
   * `update_fee_config` requires all four treasuries to share the mint it
   * is handed, so switching the allowlist to a different mint necessarily
   * switches the treasuries too — the fee is a slice of the traded amount
   * and has to land somewhere that can hold it.
   */
  function setAllowlist(
    mintPk: PublicKey,
    mints: PublicKey[],
    treasuries: { dev: PublicKey; eco: PublicKey; infra: PublicKey; emerg: PublicKey },
  ) {
    return withBlockhashRetry(() =>
      program.methods
        .updateFeeConfig({ ...SHARED_FEE_PARAMS, settlementMints: mints })
        .accountsPartial({
          admin: admin.publicKey,
          feeConfig,
          mint: mintPk,
          devTreasury: treasuries.dev,
          ecosystemTreasury: treasuries.eco,
          infraTreasury: treasuries.infra,
          emergencyReserve: treasuries.emerg,
        })
        .rpc({ commitment: "confirmed" }),
    );
  }

  before(async () => {
    sharedMint = await getSharedMint();
    ({ feeConfig, devTreasury, ecosystemTreasury, infraTreasury, emergencyReserve } =
      await getSharedFeeConfig(program));

    legacyMint = await createMint(
      connection,
      admin,
      admin.publicKey,
      null,
      MINT_DECIMALS,
      undefined,
      { commitment: "confirmed" },
      TOKEN_PROGRAM_ID,
    );
    legacyTreasuries = {
      dev: await ata(legacyMint, Keypair.generate().publicKey, TOKEN_PROGRAM_ID),
      eco: await ata(legacyMint, Keypair.generate().publicKey, TOKEN_PROGRAM_ID),
      infra: await ata(legacyMint, Keypair.generate().publicKey, TOKEN_PROGRAM_ID),
      emerg: await ata(legacyMint, Keypair.generate().publicKey, TOKEN_PROGRAM_ID),
    };
  });

  // Hand the shared singleton back exactly as the fixture left it. Later
  // suites settle in `sharedMint` through these treasuries, so a leaked
  // allowlist or a leaked treasury set breaks them, not this file.
  after(async () => {
    await setAllowlist(sharedMint, [sharedMint], {
      dev: devTreasury,
      eco: ecosystemTreasury,
      infra: infraTreasury,
      emerg: emergencyReserve,
    });
  });

  it("confirms the fixture mint is Token-2022 and the legacy mint is not", async () => {
    // The premise of everything below. If this ever flips, the "legacy"
    // cycle would be a second Token-2022 cycle wearing a different name and
    // would prove nothing — the exact failure mode that let the original
    // defect survive a full green suite.
    const shared = await connection.getAccountInfo(sharedMint, "confirmed");
    const legacy = await connection.getAccountInfo(legacyMint, "confirmed");
    expect(shared!.owner.toBase58()).to.equal(TOKEN_2022_PROGRAM_ID.toBase58());
    expect(legacy!.owner.toBase58()).to.equal(TOKEN_PROGRAM_ID.toBase58());
  });

  describe("a LEGACY SPL mint runs the full settlement cycle", () => {
    const reservationId = 90_001;
    const deposited = unit(5000);
    const amount = unit(1000);
    let merchant: Keypair;
    let buyer: Keypair;
    let vault: PublicKey;
    let tokenVault: PublicKey;
    let buyerAta: PublicKey;

    before(async () => {
      merchant = Keypair.generate();
      buyer = Keypair.generate();
      await airdrop(merchant.publicKey);
      await airdrop(buyer.publicKey);
      vault = liquidityVaultPda(merchant.publicKey, legacyMint);
      tokenVault = liquidityTokenVaultPda(merchant.publicKey, legacyMint);
      buyerAta = await ata(legacyMint, buyer.publicKey, TOKEN_PROGRAM_ID);

      await setAllowlist(legacyMint, [legacyMint], legacyTreasuries);
    });

    it("creates a liquidity vault owned by the legacy token program", async () => {
      await withBlockhashRetry(() =>
        program.methods
          .createLiquidityVault()
          .accountsPartial({
            merchant: merchant.publicKey,
            mint: legacyMint,
            liquidityVault: vault,
            tokenVault,
            tokenProgram: TOKEN_PROGRAM_ID,
            systemProgram: SystemProgram.programId,
            rent: SYSVAR_RENT_PUBKEY,
          })
          .signers([merchant])
          .rpc({ commitment: "confirmed" }),
      );

      // The vault's token account must itself be a legacy-SPL account. If
      // this is Token-2022 the migration did not take and the rest of this
      // block would be testing the old path.
      const info = await connection.getAccountInfo(tokenVault, "confirmed");
      expect(info!.owner.toBase58()).to.equal(TOKEN_PROGRAM_ID.toBase58());
    });

    it("deposits, reserves, funds, approves and releases", async () => {
      const merchantAta = await ata(legacyMint, merchant.publicKey, TOKEN_PROGRAM_ID);
      await mintTokens(legacyMint, merchantAta, deposited, TOKEN_PROGRAM_ID);

      await withBlockhashRetry(() =>
        program.methods
          .depositLiquidity(deposited)
          .accountsPartial({
            merchant: merchant.publicKey,
            liquidityVault: vault,
            tokenVault,
            from: merchantAta,
            mint: legacyMint,
            tokenProgram: TOKEN_PROGRAM_ID,
          })
          .signers([merchant])
          .rpc({ commitment: "confirmed" }),
      );

      await withBlockhashRetry(() =>
        program.methods
          .reserveLiquidity(amount)
          .accountsPartial({ merchant: merchant.publicKey, liquidityVault: vault, feeConfig })
          .signers([merchant])
          .rpc({ commitment: "confirmed" }),
      );

      const tradeEscrow = tradeEscrowPda(reservationId);
      const escrowTokenVault = tradeEscrowTokenVaultPda(reservationId);

      await withBlockhashRetry(() =>
        program.methods
          .createTradeEscrow(new BN(reservationId), amount, new BN(1800))
          .accountsPartial({
            merchant: merchant.publicKey,
            buyer: buyer.publicKey,
            mint: legacyMint,
            feeConfig,
            liquidityVault: vault,
            tradeEscrow,
            tokenVault: escrowTokenVault,
            tokenProgram: TOKEN_PROGRAM_ID,
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
            mint: legacyMint,
            liquidityVault: vault,
            liquidityTokenVault: tokenVault,
            tradeEscrow,
            tradeEscrowTokenVault: escrowTokenVault,
            tokenProgram: TOKEN_PROGRAM_ID,
          })
          .signers([merchant])
          .rpc({ commitment: "confirmed" }),
      );

      // The tokens really moved, through the legacy program.
      const escrowed = await getAccount(
        connection,
        escrowTokenVault,
        "confirmed",
        TOKEN_PROGRAM_ID,
      );
      expect(escrowed.amount.toString()).to.equal(amount.toString());

      await withBlockhashRetry(() =>
        program.methods
          .approveSettlement()
          .accountsPartial({ merchant: merchant.publicKey, tradeEscrow })
          .signers([merchant])
          .rpc({ commitment: "confirmed" }),
      );

      await withBlockhashRetry(() =>
        program.methods
          .releaseEscrow()
          .accountsPartial({
            mint: legacyMint,
            liquidityVault: vault,
            tradeEscrow,
            tradeEscrowTokenVault: escrowTokenVault,
            buyerTokenAccount: buyerAta,
            feeConfig,
            devTreasury: legacyTreasuries.dev,
            ecosystemTreasury: legacyTreasuries.eco,
            infraTreasury: legacyTreasuries.infra,
            emergencyReserve: legacyTreasuries.emerg,
            tokenProgram: TOKEN_PROGRAM_ID,
          })
          .rpc({ commitment: "confirmed" }),
      );

      // 85 bps of 1000 units, split 40/30/20/10. Asserted in full rather
      // than "buyer got something": a fee that silently failed to route
      // would still leave the buyer paid.
      const fee = amount.muln(85).divn(10_000);
      const buyerAccount = await getAccount(connection, buyerAta, "confirmed", TOKEN_PROGRAM_ID);
      expect(buyerAccount.amount.toString()).to.equal(amount.sub(fee).toString());

      const shares = [4000, 3000, 2000, 1000];
      const accounts = [
        legacyTreasuries.dev,
        legacyTreasuries.eco,
        legacyTreasuries.infra,
        legacyTreasuries.emerg,
      ];
      let routed = new BN(0);
      for (let i = 0; i < 4; i++) {
        const acc = await getAccount(connection, accounts[i], "confirmed", TOKEN_PROGRAM_ID);
        expect(acc.amount.toString()).to.equal(fee.muln(shares[i]).divn(10_000).toString());
        routed = routed.add(new BN(acc.amount.toString()));
      }
      expect(routed.toString()).to.equal(fee.toString());
    });
  });

  describe("a mint that is not on the allowlist is refused", () => {
    let strangerMint: PublicKey;
    let merchant: Keypair;

    before(async () => {
      merchant = Keypair.generate();
      await airdrop(merchant.publicKey);
      // Token-2022, so the refusal cannot be mistaken for the token
      // program being wrong rather than the mint being unlisted.
      strangerMint = await createMint(
        connection,
        admin,
        admin.publicKey,
        null,
        MINT_DECIMALS,
        undefined,
        { commitment: "confirmed" },
        TOKEN_2022_PROGRAM_ID,
      );
      await setAllowlist(legacyMint, [legacyMint], legacyTreasuries);
    });

    it("create_liquidity_vault rejects it", async () => {
      await expectAnchorError(
        withBlockhashRetry(() =>
          program.methods
            .createLiquidityVault()
            .accountsPartial({
              merchant: merchant.publicKey,
              mint: strangerMint,
              liquidityVault: liquidityVaultPda(merchant.publicKey, strangerMint),
              tokenVault: liquidityTokenVaultPda(merchant.publicKey, strangerMint),
              tokenProgram: TOKEN_2022_PROGRAM_ID,
              systemProgram: SystemProgram.programId,
              rent: SYSVAR_RENT_PUBKEY,
            })
            .signers([merchant])
            .rpc({ commitment: "confirmed" }),
        ),
        "SettlementMintNotAllowed",
      );
    });

    it("create_trade_escrow rejects it, even against a vault that already exists", async () => {
      // The point of the second gate. A vault created while its mint was
      // allowed, funded and reserved, must still not be able to open a new
      // escrow once the mint is de-listed — that is where the buyer's money
      // would land.
      const vaultMerchant = Keypair.generate();
      await airdrop(vaultMerchant.publicKey);
      const vault = liquidityVaultPda(vaultMerchant.publicKey, strangerMint);
      const tokenVault = liquidityTokenVaultPda(vaultMerchant.publicKey, strangerMint);
      const strangerTreasuries = {
        dev: await ata(strangerMint, Keypair.generate().publicKey, TOKEN_2022_PROGRAM_ID),
        eco: await ata(strangerMint, Keypair.generate().publicKey, TOKEN_2022_PROGRAM_ID),
        infra: await ata(strangerMint, Keypair.generate().publicKey, TOKEN_2022_PROGRAM_ID),
        emerg: await ata(strangerMint, Keypair.generate().publicKey, TOKEN_2022_PROGRAM_ID),
      };

      // Listed -> vault, deposit, reserve all succeed.
      await setAllowlist(strangerMint, [strangerMint], strangerTreasuries);
      await withBlockhashRetry(() =>
        program.methods
          .createLiquidityVault()
          .accountsPartial({
            merchant: vaultMerchant.publicKey,
            mint: strangerMint,
            liquidityVault: vault,
            tokenVault,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
            systemProgram: SystemProgram.programId,
            rent: SYSVAR_RENT_PUBKEY,
          })
          .signers([vaultMerchant])
          .rpc({ commitment: "confirmed" }),
      );
      const from = await ata(strangerMint, vaultMerchant.publicKey, TOKEN_2022_PROGRAM_ID);
      await mintTokens(strangerMint, from, unit(5000), TOKEN_2022_PROGRAM_ID);
      await withBlockhashRetry(() =>
        program.methods
          .depositLiquidity(unit(5000))
          .accountsPartial({
            merchant: vaultMerchant.publicKey,
            liquidityVault: vault,
            tokenVault,
            from,
            mint: strangerMint,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
          })
          .signers([vaultMerchant])
          .rpc({ commitment: "confirmed" }),
      );
      await withBlockhashRetry(() =>
        program.methods
          .reserveLiquidity(unit(1000))
          .accountsPartial({
            merchant: vaultMerchant.publicKey,
            liquidityVault: vault,
            feeConfig,
          })
          .signers([vaultMerchant])
          .rpc({ commitment: "confirmed" }),
      );

      // De-list it. The reservation stays on the books.
      await setAllowlist(legacyMint, [legacyMint], legacyTreasuries);

      const reservationId = 90_002;
      await expectAnchorError(
        withBlockhashRetry(() =>
          program.methods
            .createTradeEscrow(new BN(reservationId), unit(1000), new BN(1800))
            .accountsPartial({
              merchant: vaultMerchant.publicKey,
              buyer: Keypair.generate().publicKey,
              mint: strangerMint,
              feeConfig,
              liquidityVault: vault,
              tradeEscrow: tradeEscrowPda(reservationId),
              tokenVault: tradeEscrowTokenVaultPda(reservationId),
              tokenProgram: TOKEN_2022_PROGRAM_ID,
              systemProgram: SystemProgram.programId,
              rent: SYSVAR_RENT_PUBKEY,
            })
            .signers([vaultMerchant])
            .rpc({ commitment: "confirmed" }),
        ),
        "SettlementMintNotAllowed",
      );

      // ...and no new reservation may be taken either.
      await expectAnchorError(
        withBlockhashRetry(() =>
          program.methods
            .reserveLiquidity(unit(100))
            .accountsPartial({
              merchant: vaultMerchant.publicKey,
              liquidityVault: vault,
              feeConfig,
            })
            .signers([vaultMerchant])
            .rpc({ commitment: "confirmed" }),
        ),
        "SettlementMintNotAllowed",
      );

      // The whole point of de-listing rather than freezing: the merchant's
      // money is still theirs. 5000 deposited, 1000 reserved, so 4000 is
      // withdrawable and the withdrawal path carries no allowlist check at
      // all.
      const to = await ata(strangerMint, vaultMerchant.publicKey, TOKEN_2022_PROGRAM_ID);
      const before = await getAccount(connection, to, "confirmed", TOKEN_2022_PROGRAM_ID);
      await withBlockhashRetry(() =>
        program.methods
          .withdrawLiquidity(unit(4000))
          .accountsPartial({
            merchant: vaultMerchant.publicKey,
            liquidityVault: vault,
            tokenVault,
            to,
            mint: strangerMint,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
          })
          .signers([vaultMerchant])
          .rpc({ commitment: "confirmed" }),
      );
      const after = await getAccount(connection, to, "confirmed", TOKEN_2022_PROGRAM_ID);
      expect((after.amount - before.amount).toString()).to.equal(unit(4000).toString());
    });
  });

  describe("the supplied token program must own the mint", () => {
    // `Interface<TokenInterface>` accepts EITHER token program, so the CPI
    // target stopped being a compile-time constant the moment
    // `Program<'info, Token2022>` was replaced. On its own that is strictly
    // less safe than what it replaced: `InterfaceAccount<Mint>` happily
    // deserializes a mint from either program, so a caller could pair a
    // Token-2022 mint with the legacy program id and have transfer_checked
    // aimed at a program that does not own the accounts.
    // `mint::token_program` is what pays that back, and this block is the
    // proof it is in force.
    //
    // It deliberately uses `deposit_liquidity` rather than
    // `create_liquidity_vault`. Vault creation ALSO refuses a mismatch, but
    // for the wrong reason: its `init` on `token_vault` CPIs into the
    // supplied token program to create the account, and that CPI fails
    // first with a bare `IncorrectProgramId` from the token program itself.
    // A test written against it passes whether or not
    // `mint::token_program` exists — which is precisely the vacuous test
    // this file was written to avoid. `deposit_liquidity` creates nothing,
    // so no CPI runs during account validation and the constraint is the
    // only thing that can reject the call.
    let legacyDepositor: Keypair;
    let t22Depositor: Keypair;
    let t22Mint: PublicKey;
    let legacyFrom: PublicKey;
    let t22From: PublicKey;

    before(async () => {
      t22Mint = await createMint(
        connection,
        admin,
        admin.publicKey,
        null,
        MINT_DECIMALS,
        undefined,
        { commitment: "confirmed" },
        TOKEN_2022_PROGRAM_ID,
      );
      // Both mints allowlisted, so a rejection below can only be about the
      // token program and never about the allowlist.
      await setAllowlist(legacyMint, [legacyMint, t22Mint], legacyTreasuries);

      legacyDepositor = Keypair.generate();
      t22Depositor = Keypair.generate();
      await airdrop(legacyDepositor.publicKey);
      await airdrop(t22Depositor.publicKey);

      for (const [merchant, mintPk, tokenProgram] of [
        [legacyDepositor, legacyMint, TOKEN_PROGRAM_ID],
        [t22Depositor, t22Mint, TOKEN_2022_PROGRAM_ID],
      ] as const) {
        await withBlockhashRetry(() =>
          program.methods
            .createLiquidityVault()
            .accountsPartial({
              merchant: merchant.publicKey,
              mint: mintPk,
              liquidityVault: liquidityVaultPda(merchant.publicKey, mintPk),
              tokenVault: liquidityTokenVaultPda(merchant.publicKey, mintPk),
              tokenProgram,
              systemProgram: SystemProgram.programId,
              rent: SYSVAR_RENT_PUBKEY,
            })
            .signers([merchant])
            .rpc({ commitment: "confirmed" }),
        );
      }

      legacyFrom = await ata(legacyMint, legacyDepositor.publicKey, TOKEN_PROGRAM_ID);
      t22From = await ata(t22Mint, t22Depositor.publicKey, TOKEN_2022_PROGRAM_ID);
      await mintTokens(legacyMint, legacyFrom, unit(100), TOKEN_PROGRAM_ID);
      await mintTokens(t22Mint, t22From, unit(100), TOKEN_2022_PROGRAM_ID);
    });

    function depositWith(
      merchant: Keypair,
      mintPk: PublicKey,
      from: PublicKey,
      tokenProgram: PublicKey,
    ) {
      return withBlockhashRetry(() =>
        program.methods
          .depositLiquidity(unit(10))
          .accountsPartial({
            merchant: merchant.publicKey,
            liquidityVault: liquidityVaultPda(merchant.publicKey, mintPk),
            tokenVault: liquidityTokenVaultPda(merchant.publicKey, mintPk),
            from,
            mint: mintPk,
            tokenProgram,
          })
          .signers([merchant])
          .rpc({ commitment: "confirmed" }),
      );
    }

    it("rejects the legacy program id for a Token-2022 mint", async () => {
      await expectAnchorError(
        depositWith(t22Depositor, t22Mint, t22From, TOKEN_PROGRAM_ID),
        "ConstraintMintTokenProgram",
      );
    });

    it("rejects the Token-2022 program id for a legacy mint", async () => {
      await expectAnchorError(
        depositWith(legacyDepositor, legacyMint, legacyFrom, TOKEN_2022_PROGRAM_ID),
        "ConstraintMintTokenProgram",
      );
    });

    it("accepts each mint through its own token program", async () => {
      // The other half of the claim. Without this the two rejections above
      // would be satisfied by an instruction that refuses everything.
      await depositWith(legacyDepositor, legacyMint, legacyFrom, TOKEN_PROGRAM_ID);
      await depositWith(t22Depositor, t22Mint, t22From, TOKEN_2022_PROGRAM_ID);

      for (const [merchant, mintPk] of [
        [legacyDepositor, legacyMint],
        [t22Depositor, t22Mint],
      ] as const) {
        const vault = await program.account.liquidityVault.fetch(
          liquidityVaultPda(merchant.publicKey, mintPk),
        );
        expect(vault.total.toString()).to.equal(unit(10).toString());
      }
    });
  });

  describe("update_fee_config validates the list it is given", () => {
    const treasuries = () => legacyTreasuries;

    it("refuses an empty list", async () => {
      await expectAnchorError(
        setAllowlist(legacyMint, [], treasuries()),
        "EmptySettlementMintList",
      );
    });

    it("refuses duplicates", async () => {
      await expectAnchorError(
        setAllowlist(legacyMint, [legacyMint, legacyMint], treasuries()),
        "InvalidSettlementMint",
      );
    });

    it("refuses the default pubkey, which is the array's own padding", async () => {
      await expectAnchorError(
        setAllowlist(legacyMint, [legacyMint, PublicKey.default], treasuries()),
        "InvalidSettlementMint",
      );
    });

    it("refuses more than the array can hold", async () => {
      const tooMany = Array.from({ length: 17 }, () => Keypair.generate().publicKey);
      await expectAnchorError(
        setAllowlist(legacyMint, tooMany, treasuries()),
        "SettlementMintListFull",
      );
    });

    it("stores a full-capacity list and reports its length", async () => {
      const sixteen = [
        legacyMint,
        ...Array.from({ length: 15 }, () => Keypair.generate().publicKey),
      ];
      await setAllowlist(legacyMint, sixteen, treasuries());
      const cfg = await program.account.feeConfig.fetch(feeConfig);
      expect(cfg.settlementMintCount).to.equal(16);
      expect(cfg.settlementMints.map((m: PublicKey) => m.toBase58())).to.deep.equal(
        sixteen.map((m) => m.toBase58()),
      );

      // Shortening the list must clear the tail, not leave de-listed mints
      // readable in the account's bytes.
      await setAllowlist(legacyMint, [legacyMint], treasuries());
      const shrunk = await program.account.feeConfig.fetch(feeConfig);
      expect(shrunk.settlementMintCount).to.equal(1);
      for (let i = 1; i < 16; i++) {
        expect(shrunk.settlementMints[i].toBase58()).to.equal(
          PublicKey.default.toBase58(),
        );
      }
    });
  });
});
