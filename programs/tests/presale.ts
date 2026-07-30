import * as anchor from "@anchor-lang/core";
import { Program, BN } from "@anchor-lang/core";
import { Presale } from "../target/types/presale";
import { MockJupiter } from "../target/types/mock_jupiter";
import { Governance } from "../target/types/governance";
import { Staking } from "../target/types/staking";
import {
  TOKEN_2022_PROGRAM_ID,
  createMint,
  mintTo,
  getOrCreateAssociatedTokenAccount,
  getAccount,
} from "@solana/spl-token";
import { Keypair, PublicKey, SystemProgram, SYSVAR_RENT_PUBKEY } from "@solana/web3.js";
import { expect } from "chai";
import { getSharedGovernanceConfig } from "./shared-fixtures";
import { banWallet } from "./governance-cycle";

describe("presale", () => {
  anchor.setProvider(anchor.AnchorProvider.env());
  const provider = anchor.AnchorProvider.env();
  const connection = provider.connection;

  const program = anchor.workspace.presale as Program<Presale>;
  const mockJupiter = anchor.workspace.mock_jupiter as Program<MockJupiter>;
  const governance = anchor.workspace.governance as Program<Governance>;
  const staking = anchor.workspace.staking as Program<Staking>;

  const admin = (provider.wallet as anchor.Wallet).payer;

  const OPEN_DECIMALS = 9;
  const USDC_DECIMALS = 6;
  const openUnit = (n: number) => new BN(n).mul(new BN(10).pow(new BN(OPEN_DECIMALS)));
  const usdcUnit = (n: number) => new BN(n).mul(new BN(10).pow(new BN(USDC_DECIMALS)));

  let openMint: PublicKey;
  let usdcMint: PublicKey;
  let otherStableMint: PublicKey;
  let presaleVaultAuthority: PublicKey;
  let presaleVault: PublicKey;

  async function airdrop(pubkey: PublicKey, sol = 10) {
    const sig = await connection.requestAirdrop(pubkey, sol * 1_000_000_000);
    const latest = await connection.getLatestBlockhash();
    await connection.confirmTransaction({ signature: sig, ...latest });
  }

  // All writes below are explicitly confirmed at "confirmed" commitment —
  // matching every read in this suite — since sendAndConfirmTransaction's
  // default ("processed" on this local validator) otherwise lags visibly
  // behind an immediately-following "confirmed" read, which looked like a
  // token-transfer bug during development but was actually just this
  // commitment-level mismatch between writes and reads.
  async function ata(mint: PublicKey, owner: PublicKey, allowOwnerOffCurve = false) {
    const acc = await getOrCreateAssociatedTokenAccount(
      connection,
      admin,
      mint,
      owner,
      allowOwnerOffCurve,
      "confirmed",
      { commitment: "confirmed" },
      TOKEN_2022_PROGRAM_ID,
    );
    return acc.address;
  }

  async function mintTo9or6(mint: PublicKey, dest: PublicKey, amount: BN) {
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

  function saleConfigPda(nonce: number) {
    return PublicKey.findProgramAddressSync(
      [Buffer.from("sale_config"), new BN(nonce).toArrayLike(Buffer, "le", 8)],
      program.programId,
    )[0];
  }
  function usdcVaultPda(nonce: number) {
    return PublicKey.findProgramAddressSync(
      [Buffer.from("sale_usdc_vault"), new BN(nonce).toArrayLike(Buffer, "le", 8)],
      program.programId,
    )[0];
  }
  function contributionPda(saleConfig: PublicKey, buyer: PublicKey) {
    return PublicKey.findProgramAddressSync(
      [Buffer.from("contribution"), saleConfig.toBuffer(), buyer.toBuffer()],
      program.programId,
    )[0];
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

  /**
   * Retries a transaction send on a transient "Blockhash not found"
   * simulation error — an occasional local-validator/RPC race where the
   * blockhash used to build the transaction rotates out between signing
   * and send. A known, standard mitigation for Solana transaction
   * submission (not specific to this program's logic); rethrows anything
   * else immediately.
   */
  async function withBlockhashRetry<T>(
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

  /**
   * The validator's own clock, which is what `finalize_sale` compares
   * `end_time` against — not this process's wall clock.
   *
   * The two drift apart. On-chain time advances with slot production, so a
   * loaded runner falls behind real time, and the gap grows with how long the
   * suite has already been running. Every sale window here is still
   * *written* from `Date.now()`, which is fine for the hour-long windows, but
   * the two blocks that deliberately use a two-second window then waited a
   * fixed 2.5s of wall time and assumed the sale had ended. Under enough
   * drift it had not, and `finalize_sale` failed with the sale still open —
   * a failure that appears in an unrelated part of the suite whenever
   * anything earlier gets slower.
   */
  async function onchainNow(): Promise<number> {
    const slot = await connection.getSlot("confirmed");
    const time = await connection.getBlockTime(slot);
    if (time === null) throw new Error(`no block time for slot ${slot}`);
    return time;
  }

  /** Polls until the validator's clock has passed `target`. */
  async function waitForOnchainTime(target: number) {
    for (let i = 0; i < 240; i++) {
      if ((await onchainNow()) > target) return;
      await new Promise((r) => setTimeout(r, 500));
    }
    throw new Error(`on-chain clock did not pass ${target} within 120s`);
  }

  type SaleParams = {
    hardCap: BN;
    softCap: BN;
    minContribution: BN;
    maxContribution: BN;
    maxSlippageBps: number;
    startTime: BN;
    endTime: BN;
    stablecoinWhitelist: PublicKey[];
  };

  async function initSale(
    nonce: number,
    params: SaleParams,
    treasury: PublicKey,
    swapProgram: PublicKey,
  ) {
    const saleConfig = saleConfigPda(nonce);
    const usdcVault = usdcVaultPda(nonce);
    await program.methods
      .initializeSale(new BN(nonce), params)
      .accountsPartial({
        admin: admin.publicKey,
        saleConfig,
        openMint,
        usdcMint,
        presaleVaultAuthority,
        presaleVault,
        usdcVault,
        treasury,
        swapProgram,
        tokenProgram: TOKEN_2022_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
        rent: SYSVAR_RENT_PUBKEY,
      })
      .rpc({ commitment: "confirmed" });
    return { saleConfig, usdcVault };
  }

  before(async () => {
    openMint = await createMint(
      connection,
      admin,
      admin.publicKey,
      null,
      OPEN_DECIMALS,
      undefined,
      { commitment: "confirmed" },
      TOKEN_2022_PROGRAM_ID,
    );
    usdcMint = await createMint(
      connection,
      admin,
      admin.publicKey,
      null,
      USDC_DECIMALS,
      undefined,
      { commitment: "confirmed" },
      TOKEN_2022_PROGRAM_ID,
    );
    otherStableMint = await createMint(
      connection,
      admin,
      admin.publicKey,
      null,
      USDC_DECIMALS,
      undefined,
      { commitment: "confirmed" },
      TOKEN_2022_PROGRAM_ID,
    );

    [presaleVaultAuthority] = PublicKey.findProgramAddressSync(
      [Buffer.from("presale_vault")],
      program.programId,
    );
    presaleVault = await ata(openMint, presaleVaultAuthority, true);
    await mintTo9or6(openMint, presaleVault, openUnit(1_000_000));
  });

  describe("initialize_sale", () => {
    it("creates the sale config singleton for a given nonce", async () => {
      const treasury = await ata(usdcMint, admin.publicKey);
      const now = Math.floor(Date.now() / 1000);
      const { saleConfig } = await initSale(
        100,
        {
          hardCap: usdcUnit(1_000_000),
          softCap: usdcUnit(1),
          minContribution: usdcUnit(1),
          maxContribution: usdcUnit(1_000_000),
          maxSlippageBps: 100,
          startTime: new BN(now - 60),
          endTime: new BN(now + 3600),
          stablecoinWhitelist: [otherStableMint],
        },
        treasury,
        mockJupiter.programId,
      );
      const account = await program.account.saleConfig.fetch(saleConfig);
      expect(account.admin.toBase58()).to.equal(admin.publicKey.toBase58());
      expect(account.state).to.deep.equal({ active: {} });
      expect(account.totalRaised.toNumber()).to.equal(0);
    });
  });

  describe("contribute_usdc limits", () => {
    const nonce = 200;
    let saleConfig: PublicKey;
    let usdcVault: PublicKey;
    let buyer: Keypair;
    let buyerUsdc: PublicKey;

    before(async () => {
      const treasury = await ata(usdcMint, admin.publicKey);
      const now = Math.floor(Date.now() / 1000);
      ({ saleConfig, usdcVault } = await initSale(
        nonce,
        {
          hardCap: usdcUnit(150),
          softCap: usdcUnit(1),
          minContribution: usdcUnit(50),
          maxContribution: usdcUnit(100),
          maxSlippageBps: 100,
          startTime: new BN(now - 60),
          endTime: new BN(now + 3600),
          stablecoinWhitelist: [],
        },
        treasury,
        mockJupiter.programId,
      ));

      buyer = Keypair.generate();
      await airdrop(buyer.publicKey);
      buyerUsdc = await ata(usdcMint, buyer.publicKey);
      await mintTo9or6(usdcMint, buyerUsdc, usdcUnit(1_000));
    });

    it("refuses a contribution from a banned wallet (OFS-7100 §12)", async () => {
      // The presale's `usdc_vault` is a vault like any other, so §12's
      // "MUST NOT be able to deposit into any vault" reaches it. Listed
      // in `governance`, refused here, with no presale-side registry —
      // the same single ban record the escrow and staking gates read.
      await getSharedGovernanceConfig(governance);
      const banned = Keypair.generate();
      await airdrop(banned.publicKey);
      const bannedUsdc = await ata(usdcMint, banned.publicKey);
      await mintTo9or6(usdcMint, bannedUsdc, usdcUnit(1_000));

      // Listed through the machinery that can actually list: a
      // Standards proposal naming this wallet, carried past quorum,
      // tallied, and executed once its vote lock elapsed. There is no
      // admin shortcut left to take here.
      await banWallet(governance, staking, banned.publicKey, { sanctions: {} }, [
        ...Buffer.alloc(32, 7),
      ]);

      await expectAnchorError(
        program.methods
          .contributeUsdc(new BN(nonce), usdcUnit(60))
          .accountsPartial({
            buyer: banned.publicKey,
            saleConfig,
            buyerUsdc: bannedUsdc,
            usdcVault,
            usdcMint,
            contribution: contributionPda(saleConfig, banned.publicKey),
            tokenProgram: TOKEN_2022_PROGRAM_ID,
            systemProgram: SystemProgram.programId,
          })
          .signers([banned])
          .rpc({ commitment: "confirmed" }),
        "WalletBanned",
      );
    });

    it("rejects a first contribution below the minimum", async () => {
      const contribution = contributionPda(saleConfig, buyer.publicKey);
      await expectAnchorError(
        program.methods
          .contributeUsdc(new BN(nonce), usdcUnit(10))
          .accountsPartial({
            buyer: buyer.publicKey,
            saleConfig,
            buyerUsdc,
            usdcVault,
            usdcMint,
            contribution,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
            systemProgram: SystemProgram.programId,
          })
          .signers([buyer])
          .rpc({ commitment: "confirmed" }),
        "BelowMinimumContribution",
      );
    });

    it("accepts a valid contribution and records entitlement", async () => {
      const contribution = contributionPda(saleConfig, buyer.publicKey);
      const vaultBefore = await getAccount(
        connection,
        usdcVault,
        "confirmed",
        TOKEN_2022_PROGRAM_ID,
      );
      await program.methods
        .contributeUsdc(new BN(nonce), usdcUnit(60))
        .accountsPartial({
          buyer: buyer.publicKey,
          saleConfig,
          buyerUsdc,
          usdcVault,
          usdcMint,
          contribution,
          tokenProgram: TOKEN_2022_PROGRAM_ID,
          systemProgram: SystemProgram.programId,
        })
        .signers([buyer])
        .rpc({ commitment: "confirmed" });
      const vaultAfter = await getAccount(
        connection,
        usdcVault,
        "confirmed",
        TOKEN_2022_PROGRAM_ID,
      );
      expect((vaultAfter.amount - vaultBefore.amount).toString()).to.equal(
        usdcUnit(60).toString(),
      );

      const acc = await program.account.contribution.fetch(contribution);
      expect(acc.amountUsdc.toString()).to.equal(usdcUnit(60).toString());
      expect(acc.openEntitlement.toString()).to.equal(openUnit(60).toString());
    });

    it("rejects a follow-up contribution that exceeds the per-wallet maximum", async () => {
      const contribution = contributionPda(saleConfig, buyer.publicKey);
      await expectAnchorError(
        program.methods
          .contributeUsdc(new BN(nonce), usdcUnit(50))
          .accountsPartial({
            buyer: buyer.publicKey,
            saleConfig,
            buyerUsdc,
            usdcVault,
            usdcMint,
            contribution,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
            systemProgram: SystemProgram.programId,
          })
          .signers([buyer])
          .rpc({ commitment: "confirmed" }),
        "AboveMaximumContribution",
      );
    });

    it("rejects a contribution (from a second wallet) that would exceed the hard cap", async () => {
      const buyer2 = Keypair.generate();
      await airdrop(buyer2.publicKey);
      const buyer2Usdc = await ata(usdcMint, buyer2.publicKey);
      await mintTo9or6(usdcMint, buyer2Usdc, usdcUnit(1_000));
      const contribution2 = contributionPda(saleConfig, buyer2.publicKey);

      // Sale already holds 60 USDC (from buyer 1); hard cap is 150 — a 100
      // USDC contribution would push total_raised to 160 > 150.
      await expectAnchorError(
        program.methods
          .contributeUsdc(new BN(nonce), usdcUnit(100))
          .accountsPartial({
            buyer: buyer2.publicKey,
            saleConfig,
            buyerUsdc: buyer2Usdc,
            usdcVault,
            usdcMint,
            contribution: contribution2,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
            systemProgram: SystemProgram.programId,
          })
          .signers([buyer2])
          .rpc({ commitment: "confirmed" }),
        "HardCapExceeded",
      );
    });
  });

  describe("update_sale_params", () => {
    const nonce = 150;
    let saleConfig: PublicKey;

    before(async () => {
      const treasury = await ata(usdcMint, admin.publicKey);
      const now = Math.floor(Date.now() / 1000);
      ({ saleConfig } = await initSale(
        nonce,
        {
          hardCap: usdcUnit(1_000),
          softCap: usdcUnit(10),
          minContribution: usdcUnit(1),
          maxContribution: usdcUnit(500),
          maxSlippageBps: 100,
          startTime: new BN(now - 60),
          endTime: new BN(now + 3600),
          stablecoinWhitelist: [],
        },
        treasury,
        mockJupiter.programId,
      ));
    });

    it("rejects an update from a non-admin signer", async () => {
      const impostor = Keypair.generate();
      await airdrop(impostor.publicKey);
      await expectAnchorError(
        withBlockhashRetry(() =>
          program.methods
            .updateSaleParams(new BN(nonce), {
              hardCap: usdcUnit(2_000),
              softCap: usdcUnit(10),
              minContribution: usdcUnit(1),
              maxContribution: usdcUnit(1_000),
              maxSlippageBps: 100,
              endTime: new BN(Math.floor(Date.now() / 1000) + 3600),
            })
            .accountsPartial({ admin: impostor.publicKey, saleConfig })
            .signers([impostor])
            .rpc({ commitment: "confirmed" }),
        ),
        "Unauthorized",
      );
    });

    it("lets the admin raise the hard cap and per-wallet max", async () => {
      await withBlockhashRetry(() =>
        program.methods
          .updateSaleParams(new BN(nonce), {
            hardCap: usdcUnit(30_000_000),
            softCap: usdcUnit(5_000_000),
            minContribution: usdcUnit(1),
            maxContribution: usdcUnit(1_000_000),
            maxSlippageBps: 100,
            endTime: new BN(Math.floor(Date.now() / 1000) + 3600),
          })
          .accountsPartial({ admin: admin.publicKey, saleConfig })
          .rpc({ commitment: "confirmed" }),
      );

      const account = await program.account.saleConfig.fetch(saleConfig);
      expect(account.hardCap.toString()).to.equal(
        usdcUnit(30_000_000).toString(),
      );
      expect(account.maxContribution.toString()).to.equal(
        usdcUnit(1_000_000).toString(),
      );
    });

    it("rejects a hard cap that isn't greater than the soft cap", async () => {
      await expectAnchorError(
        program.methods
          .updateSaleParams(new BN(nonce), {
            hardCap: usdcUnit(5),
            softCap: usdcUnit(10),
            minContribution: usdcUnit(1),
            maxContribution: usdcUnit(1_000),
            maxSlippageBps: 100,
            endTime: new BN(Math.floor(Date.now() / 1000) + 3600),
          })
          .accountsPartial({ admin: admin.publicKey, saleConfig })
          .rpc({ commitment: "confirmed" }),
        "HardCapNotGreaterThanSoftCap",
      );
    });
  });

  describe("sale window enforcement", () => {
    it("rejects a contribution before the sale's start_time", async () => {
      const nonce = 300;
      const treasury = await ata(usdcMint, admin.publicKey);
      const now = Math.floor(Date.now() / 1000);
      const { saleConfig, usdcVault } = await initSale(
        nonce,
        {
          hardCap: usdcUnit(1_000),
          softCap: usdcUnit(1),
          minContribution: usdcUnit(1),
          maxContribution: usdcUnit(1_000),
          maxSlippageBps: 100,
          startTime: new BN(now + 3600),
          endTime: new BN(now + 7200),
          stablecoinWhitelist: [],
        },
        treasury,
        mockJupiter.programId,
      );
      const buyer = Keypair.generate();
      await airdrop(buyer.publicKey);
      const buyerUsdc = await ata(usdcMint, buyer.publicKey);
      await mintTo9or6(usdcMint, buyerUsdc, usdcUnit(100));
      const contribution = contributionPda(saleConfig, buyer.publicKey);

      await expectAnchorError(
        program.methods
          .contributeUsdc(new BN(nonce), usdcUnit(10))
          .accountsPartial({
            buyer: buyer.publicKey,
            saleConfig,
            buyerUsdc,
            usdcVault,
            usdcMint,
            contribution,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
            systemProgram: SystemProgram.programId,
          })
          .signers([buyer])
          .rpc({ commitment: "confirmed" }),
        "SaleNotStarted",
      );
    });

    it("rejects a contribution after the sale's end_time", async () => {
      const nonce = 301;
      const treasury = await ata(usdcMint, admin.publicKey);
      const now = Math.floor(Date.now() / 1000);
      const { saleConfig, usdcVault } = await initSale(
        nonce,
        {
          hardCap: usdcUnit(1_000),
          softCap: usdcUnit(1),
          minContribution: usdcUnit(1),
          maxContribution: usdcUnit(1_000),
          maxSlippageBps: 100,
          startTime: new BN(now - 7200),
          endTime: new BN(now - 3600),
          stablecoinWhitelist: [],
        },
        treasury,
        mockJupiter.programId,
      );
      const buyer = Keypair.generate();
      await airdrop(buyer.publicKey);
      const buyerUsdc = await ata(usdcMint, buyer.publicKey);
      await mintTo9or6(usdcMint, buyerUsdc, usdcUnit(100));
      const contribution = contributionPda(saleConfig, buyer.publicKey);

      await expectAnchorError(
        program.methods
          .contributeUsdc(new BN(nonce), usdcUnit(10))
          .accountsPartial({
            buyer: buyer.publicKey,
            saleConfig,
            buyerUsdc,
            usdcVault,
            usdcMint,
            contribution,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
            systemProgram: SystemProgram.programId,
          })
          .signers([buyer])
          .rpc({ commitment: "confirmed" }),
        "SaleEnded",
      );
    });
  });

  describe("contribute_with_swap via mock-jupiter", () => {
    const nonce = 400;
    let saleConfig: PublicKey;
    let usdcVault: PublicKey;
    let buyer: Keypair;
    let buyerOther: PublicKey;
    let reserveAuthority: PublicKey;
    let reserve: PublicKey;
    let sink: PublicKey;

    before(async () => {
      const treasury = await ata(usdcMint, admin.publicKey);
      const now = Math.floor(Date.now() / 1000);
      ({ saleConfig, usdcVault } = await initSale(
        nonce,
        {
          hardCap: usdcUnit(1_000),
          softCap: usdcUnit(1),
          minContribution: usdcUnit(10),
          maxContribution: usdcUnit(500),
          maxSlippageBps: 100, // 1%
          startTime: new BN(now - 60),
          endTime: new BN(now + 3600),
          stablecoinWhitelist: [otherStableMint],
        },
        treasury,
        mockJupiter.programId,
      ));

      buyer = Keypair.generate();
      await airdrop(buyer.publicKey);
      buyerOther = await ata(otherStableMint, buyer.publicKey);
      await mintTo9or6(otherStableMint, buyerOther, usdcUnit(1_000));

      [reserveAuthority] = PublicKey.findProgramAddressSync(
        [Buffer.from("reserve")],
        mockJupiter.programId,
      );
      reserve = await ata(usdcMint, reserveAuthority, true);
      await mintTo9or6(usdcMint, reserve, usdcUnit(10_000));
      sink = await ata(otherStableMint, admin.publicKey);
    });

    it("rejects a swap whose realized output is below the required slippage floor", async () => {
      const contribution = contributionPda(saleConfig, buyer.publicKey);
      const amountIn = usdcUnit(100);
      const expectedOut = usdcUnit(100);
      // Configure the mock to return far less than the 1%-slippage floor
      // (99 USDC) — e.g. 50 USDC, simulating a bad/manipulated route.
      const badAmountOut = usdcUnit(50);

      const swapIx = await mockJupiter.methods
        .mockSwap(amountIn, badAmountOut)
        .accountsPartial({
          sourceAuthority: buyer.publicKey,
          source: buyerOther,
          sourceMint: otherStableMint,
          sink,
          destination: usdcVault,
          destinationMint: usdcMint,
          reserve,
          reserveAuthority,
          tokenProgram: TOKEN_2022_PROGRAM_ID,
        })
        .instruction();

      await expectAnchorError(
        program.methods
          .contributeWithSwap(new BN(nonce), expectedOut, swapIx.data)
          .accountsPartial({
            buyer: buyer.publicKey,
            saleConfig,
            sourceMint: otherStableMint,
            usdcVault,
            contribution,
            swapProgram: mockJupiter.programId,
            systemProgram: SystemProgram.programId,
          })
          .remainingAccounts(swapIx.keys)
          .signers([buyer])
          .rpc({ commitment: "confirmed" }),
        "SlippageExceeded",
      );
    });

    it("converts a whitelisted stablecoin to USDC atomically and credits entitlement", async () => {
      const contribution = contributionPda(saleConfig, buyer.publicKey);
      const amountIn = usdcUnit(100);
      const expectedOut = usdcUnit(100);
      const actualOut = usdcUnit(99); // within the 1% slippage floor

      const usdcVaultBefore = await getAccount(
        connection,
        usdcVault,
        "confirmed",
        TOKEN_2022_PROGRAM_ID,
      );

      const swapIx = await mockJupiter.methods
        .mockSwap(amountIn, actualOut)
        .accountsPartial({
          sourceAuthority: buyer.publicKey,
          source: buyerOther,
          sourceMint: otherStableMint,
          sink,
          destination: usdcVault,
          destinationMint: usdcMint,
          reserve,
          reserveAuthority,
          tokenProgram: TOKEN_2022_PROGRAM_ID,
        })
        .instruction();

      await program.methods
        .contributeWithSwap(new BN(nonce), expectedOut, swapIx.data)
        .accountsPartial({
          buyer: buyer.publicKey,
          saleConfig,
          sourceMint: otherStableMint,
          usdcVault,
          contribution,
          swapProgram: mockJupiter.programId,
          systemProgram: SystemProgram.programId,
        })
        .remainingAccounts(swapIx.keys)
        .signers([buyer])
        .rpc({ commitment: "confirmed" });

      const usdcVaultAfter = await getAccount(
        connection,
        usdcVault,
        "confirmed",
        TOKEN_2022_PROGRAM_ID,
      );
      expect(
        (usdcVaultAfter.amount - usdcVaultBefore.amount).toString(),
      ).to.equal(actualOut.toString());

      const acc = await program.account.contribution.fetch(contribution);
      expect(acc.amountUsdc.toString()).to.equal(actualOut.toString());
      expect(acc.openEntitlement.toString()).to.equal(openUnit(99).toString());
    });

    it("rejects a swap CPI targeting a program other than sale_config.swap_program", async () => {
      const contribution = contributionPda(saleConfig, buyer.publicKey);
      await expectAnchorError(
        program.methods
          .contributeWithSwap(new BN(nonce), usdcUnit(10), Buffer.from([]))
          .accountsPartial({
            buyer: buyer.publicKey,
            saleConfig,
            sourceMint: otherStableMint,
            usdcVault,
            contribution,
            swapProgram: program.programId, // wrong program on purpose
            systemProgram: SystemProgram.programId,
          })
          .signers([buyer])
          .rpc({ commitment: "confirmed" }),
        "SwapProgramMismatch",
      );
    });
  });

  describe("finalize_sale + claim (soft cap met)", () => {
    const nonce = 500;
    let saleConfig: PublicKey;
    let usdcVault: PublicKey;
    let treasury: PublicKey;
    let buyer: Keypair;
    let buyerUsdc: PublicKey;
    let buyerOpen: PublicKey;
    // Describe-scoped: the finalize spec below needs the same value the
    // `before` hook created the sale with.
    let saleEndsAt: number;

    before(async () => {
      treasury = await ata(usdcMint, Keypair.generate().publicKey);
      const now = Math.floor(Date.now() / 1000);
      saleEndsAt = (await onchainNow()) + 12;
      ({ saleConfig, usdcVault } = await initSale(
        nonce,
        {
          hardCap: usdcUnit(1_000),
          softCap: usdcUnit(50),
          minContribution: usdcUnit(10),
          maxContribution: usdcUnit(1_000),
          maxSlippageBps: 100,
          startTime: new BN(now - 60),
          // Derived from the validator's own clock, not this process's, and 12
          // seconds rather than 2 so the contribution below still lands inside
          // the window on a slow runner. `waitForOnchainTime` is what makes
          // the wait deterministic, so a wider window costs no extra time.
          endTime: new BN(saleEndsAt),
          stablecoinWhitelist: [],
        },
        treasury,
        mockJupiter.programId,
      ));

      buyer = Keypair.generate();
      await airdrop(buyer.publicKey);
      buyerUsdc = await ata(usdcMint, buyer.publicKey);
      await mintTo9or6(usdcMint, buyerUsdc, usdcUnit(1_000));
      buyerOpen = await ata(openMint, buyer.publicKey);

      const contribution = contributionPda(saleConfig, buyer.publicKey);
      await program.methods
        .contributeUsdc(new BN(nonce), usdcUnit(80)) // clears the 50 USDC soft cap
        .accountsPartial({
          buyer: buyer.publicKey,
          saleConfig,
          buyerUsdc,
          usdcVault,
          usdcMint,
          contribution,
          tokenProgram: TOKEN_2022_PROGRAM_ID,
          systemProgram: SystemProgram.programId,
        })
        .signers([buyer])
        .rpc({ commitment: "confirmed" });
    });

    it("rejects finalize before the sale has ended and the hard cap hasn't been hit", async () => {
      await expectAnchorError(
        program.methods
          .finalizeSale(new BN(nonce))
          .accountsPartial({
            admin: admin.publicKey,
            saleConfig,
            usdcVault,
            treasury,
            usdcMint,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
          })
          .rpc({ commitment: "confirmed" }),
        "SaleNotEnded",
      );
    });

    it("rejects claim before the sale is finalized", async () => {
      const contribution = contributionPda(saleConfig, buyer.publicKey);
      await expectAnchorError(
        program.methods
          .claim(new BN(nonce))
          .accountsPartial({
            buyer: buyer.publicKey,
            saleConfig,
            openMint,
            presaleVaultAuthority,
            presaleVault,
            contribution,
            buyerOpen,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
          })
          .signers([buyer])
          .rpc({ commitment: "confirmed" }),
        "SaleNotFinalized",
      );
    });

    it("finalizes the sale (soft cap met) and sweeps USDC to the treasury", async () => {
      await waitForOnchainTime(saleEndsAt);

      await program.methods
        .finalizeSale(new BN(nonce))
        .accountsPartial({
          admin: admin.publicKey,
          saleConfig,
          usdcVault,
          treasury,
          usdcMint,
          tokenProgram: TOKEN_2022_PROGRAM_ID,
        })
        .rpc({ commitment: "confirmed" });

      const config = await program.account.saleConfig.fetch(saleConfig);
      expect(config.state).to.deep.equal({ finalized: {} });

      const treasuryAcc = await getAccount(
        connection,
        treasury,
        "confirmed",
        TOKEN_2022_PROGRAM_ID,
      );
      expect(treasuryAcc.amount.toString()).to.equal(usdcUnit(80).toString());
    });

    it("rejects a second finalize once already resolved", async () => {
      await expectAnchorError(
        program.methods
          .finalizeSale(new BN(nonce))
          .accountsPartial({
            admin: admin.publicKey,
            saleConfig,
            usdcVault,
            treasury,
            usdcMint,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
          })
          .rpc({ commitment: "confirmed" }),
        "SaleAlreadyResolved",
      );
    });

    it("lets the buyer claim their OPEN entitlement", async () => {
      const contribution = contributionPda(saleConfig, buyer.publicKey);
      await program.methods
        .claim(new BN(nonce))
        .accountsPartial({
          buyer: buyer.publicKey,
          saleConfig,
          openMint,
          presaleVaultAuthority,
          presaleVault,
          contribution,
          buyerOpen,
          tokenProgram: TOKEN_2022_PROGRAM_ID,
        })
        .signers([buyer])
        .rpc({ commitment: "confirmed" });

      const openAcc = await getAccount(
        connection,
        buyerOpen,
        "confirmed",
        TOKEN_2022_PROGRAM_ID,
      );
      expect(openAcc.amount.toString()).to.equal(openUnit(80).toString());
    });

    it("rejects a second claim on the same contribution", async () => {
      const contribution = contributionPda(saleConfig, buyer.publicKey);
      await expectAnchorError(
        program.methods
          .claim(new BN(nonce))
          .accountsPartial({
            buyer: buyer.publicKey,
            saleConfig,
            openMint,
            presaleVaultAuthority,
            presaleVault,
            contribution,
            buyerOpen,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
          })
          .signers([buyer])
          .rpc({ commitment: "confirmed" }),
        "AlreadyClaimed",
      );
    });

    it("rejects refund on a sale that finalized successfully (not soft-cap-missed)", async () => {
      const contribution = contributionPda(saleConfig, buyer.publicKey);
      await expectAnchorError(
        program.methods
          .refund(new BN(nonce))
          .accountsPartial({
            buyer: buyer.publicKey,
            saleConfig,
            usdcVault,
            usdcMint,
            contribution,
            buyerUsdc,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
          })
          .signers([buyer])
          .rpc({ commitment: "confirmed" }),
        "SaleNotRefundable",
      );
    });
  });

  describe("finalize_sale + refund (soft cap missed)", () => {
    const nonce = 600;
    let saleConfig: PublicKey;
    let usdcVault: PublicKey;
    let treasury: PublicKey;
    let buyer: Keypair;
    let buyerUsdc: PublicKey;

    before(async () => {
      treasury = await ata(usdcMint, Keypair.generate().publicKey);
      const now = Math.floor(Date.now() / 1000);
      const saleEndsAt = (await onchainNow()) + 12;
      ({ saleConfig, usdcVault } = await initSale(
        nonce,
        {
          hardCap: usdcUnit(1_000),
          softCap: usdcUnit(500), // far above what we'll actually raise
          minContribution: usdcUnit(10),
          maxContribution: usdcUnit(1_000),
          maxSlippageBps: 100,
          startTime: new BN(now - 60),
          // See the soft-cap-met block above: on-chain clock, wider window.
          endTime: new BN(saleEndsAt),
          stablecoinWhitelist: [],
        },
        treasury,
        mockJupiter.programId,
      ));

      buyer = Keypair.generate();
      await airdrop(buyer.publicKey);
      buyerUsdc = await ata(usdcMint, buyer.publicKey);
      await mintTo9or6(usdcMint, buyerUsdc, usdcUnit(1_000));

      const contribution = contributionPda(saleConfig, buyer.publicKey);
      await program.methods
        .contributeUsdc(new BN(nonce), usdcUnit(40)) // well under the 500 USDC soft cap
        .accountsPartial({
          buyer: buyer.publicKey,
          saleConfig,
          buyerUsdc,
          usdcVault,
          usdcMint,
          contribution,
          tokenProgram: TOKEN_2022_PROGRAM_ID,
          systemProgram: SystemProgram.programId,
        })
        .signers([buyer])
        .rpc({ commitment: "confirmed" });

      await waitForOnchainTime(saleEndsAt);
      await program.methods
        .finalizeSale(new BN(nonce))
        .accountsPartial({
          admin: admin.publicKey,
          saleConfig,
          usdcVault,
          treasury,
          usdcMint,
          tokenProgram: TOKEN_2022_PROGRAM_ID,
        })
        .rpc({ commitment: "confirmed" });
    });

    it("resolves to SoftCapMissed rather than Finalized", async () => {
      const config = await program.account.saleConfig.fetch(saleConfig);
      expect(config.state).to.deep.equal({ softCapMissed: {} });
    });

    it("does not sweep any USDC to the treasury", async () => {
      const treasuryAcc = await getAccount(
        connection,
        treasury,
        "confirmed",
        TOKEN_2022_PROGRAM_ID,
      );
      expect(treasuryAcc.amount.toString()).to.equal("0");
    });

    it("refunds the buyer's USDC in full", async () => {
      const before = await getAccount(
        connection,
        buyerUsdc,
        "confirmed",
        TOKEN_2022_PROGRAM_ID,
      );
      const contribution = contributionPda(saleConfig, buyer.publicKey);
      await program.methods
        .refund(new BN(nonce))
        .accountsPartial({
          buyer: buyer.publicKey,
          saleConfig,
          usdcVault,
          usdcMint,
          contribution,
          buyerUsdc,
          tokenProgram: TOKEN_2022_PROGRAM_ID,
        })
        .signers([buyer])
        .rpc({ commitment: "confirmed" });

      const after = await getAccount(
        connection,
        buyerUsdc,
        "confirmed",
        TOKEN_2022_PROGRAM_ID,
      );
      expect((after.amount - before.amount).toString()).to.equal(
        usdcUnit(40).toString(),
      );
    });

    it("rejects a second refund on the same contribution", async () => {
      const contribution = contributionPda(saleConfig, buyer.publicKey);
      await expectAnchorError(
        program.methods
          .refund(new BN(nonce))
          .accountsPartial({
            buyer: buyer.publicKey,
            saleConfig,
            usdcVault,
            usdcMint,
            contribution,
            buyerUsdc,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
          })
          .signers([buyer])
          .rpc({ commitment: "confirmed" }),
        "AlreadyRefunded",
      );
    });
  });
});
