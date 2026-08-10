import * as anchor from "@anchor-lang/core";
import { Program, BN } from "@anchor-lang/core";
import { Presale } from "../target/types/presale";
import { MockJupiter } from "../target/types/mock_jupiter";
import { Governance } from "../target/types/governance";
import { Staking } from "../target/types/staking";
import {
  TOKEN_2022_PROGRAM_ID,
  ASSOCIATED_TOKEN_PROGRAM_ID,
  createMint,
  mintTo,
  getOrCreateAssociatedTokenAccount,
  getAssociatedTokenAddressSync,
  getAccount,
} from "@solana/spl-token";
import { Keypair, PublicKey, SystemProgram } from "@solana/web3.js";
import { expect } from "chai";
import { getSharedGovernanceConfig } from "./shared-fixtures";
import { banWallet } from "./governance-cycle";

// `deliver_contribution` is the deBridge-Hook-facing entry point (SP-B):
// the `payer` (deBridge executor) signs and funds everything, but the
// `recipient` — a plain, non-signer `Pubkey` instruction arg — is who the
// `Contribution` record belongs to and who the delivered OPEN lands with.
// These specs exist to pin down exactly the invariants that make that safe:
// no OPEN is ever credited without a matching, *measured* USDC transfer
// landing in `usdc_vault` in this same instruction (no free mint, even
// across a mid-sale `sweep_proceeds`), a malicious payer cannot redirect
// the OPEN to itself, and the usual min/max/hard_cap/ban gates apply to the
// recipient exactly as they do to a direct `contribute_usdc` buyer.
describe("presale deliver_contribution", () => {
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
  let presaleVaultAuthority: PublicKey;
  let presaleVault: PublicKey;

  async function airdrop(pubkey: PublicKey, sol = 10) {
    const sig = await connection.requestAirdrop(pubkey, sol * 1_000_000_000);
    const latest = await connection.getLatestBlockhash();
    await connection.confirmTransaction({ signature: sig, ...latest });
  }

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

  async function mintUsdc(dest: PublicKey, amount: BN) {
    await mintTo(
      connection,
      admin,
      usdcMint,
      dest,
      admin,
      BigInt(amount.toString()),
      [],
      { commitment: "confirmed" },
      TOKEN_2022_PROGRAM_ID,
    );
  }

  async function mintOpen(dest: PublicKey, amount: BN) {
    await mintTo(
      connection,
      admin,
      openMint,
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
  function contributionPda(saleConfig: PublicKey, recipient: PublicKey) {
    return PublicKey.findProgramAddressSync(
      [Buffer.from("contribution"), saleConfig.toBuffer(), recipient.toBuffer()],
      program.programId,
    )[0];
  }
  function recipientOpenAta(recipient: PublicKey) {
    return getAssociatedTokenAddressSync(
      openMint,
      recipient,
      true,
      TOKEN_2022_PROGRAM_ID,
    );
  }

  async function expectAnyRejection(p: Promise<unknown>) {
    try {
      await p;
      expect.fail("expected instruction to fail, but it succeeded");
    } catch {
      // any failure is acceptable here — some of these cases fail at the
      // SPL token program (raw, no Anchor error code), not at a `require!`.
    }
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

  async function contributionExists(pda: PublicKey): Promise<boolean> {
    try {
      await program.account.contribution.fetch(pda);
      return true;
    } catch {
      return false;
    }
  }

  type SaleParams = {
    hardCap: BN;
    softCap: BN;
    minContribution: BN;
    maxContribution: BN;
    maxSlippageBps: number;
    openPerUsdc: BN;
    startTime: BN;
    endTime: BN;
    stablecoinWhitelist: PublicKey[];
  };

  async function initSale(nonce: number, params: SaleParams, treasury: PublicKey) {
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
        swapProgram: mockJupiter.programId,
        tokenProgram: TOKEN_2022_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .rpc({ commitment: "confirmed" });
    return { saleConfig, usdcVault };
  }

  /** Builds a ready-to-send `deliverContribution` call with every account
   *  wired up correctly; individual specs override just what they're
   *  testing. */
  function deliverBuilder(opts: {
    nonce: number;
    saleConfig: PublicKey;
    usdcVault: PublicKey;
    payer: Keypair;
    sourceUsdc: PublicKey;
    recipient: PublicKey;
    recipientAccount?: PublicKey;
    recipientOpen?: PublicKey;
    usdcAmount: BN;
  }) {
    const contribution = contributionPda(opts.saleConfig, opts.recipient);
    return program.methods
      .deliverContribution(new BN(opts.nonce), opts.recipient, opts.usdcAmount)
      .accountsPartial({
        payer: opts.payer.publicKey,
        saleConfig: opts.saleConfig,
        usdcMint,
        sourceUsdc: opts.sourceUsdc,
        usdcVault: opts.usdcVault,
        openMint,
        presaleVaultAuthority,
        presaleVault,
        recipientAccount: opts.recipientAccount ?? opts.recipient,
        recipientOpen: opts.recipientOpen ?? recipientOpenAta(opts.recipient),
        contribution,
        usdcTokenProgram: TOKEN_2022_PROGRAM_ID,
        openTokenProgram: TOKEN_2022_PROGRAM_ID,
        associatedTokenProgram: ASSOCIATED_TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .signers([opts.payer]);
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

    [presaleVaultAuthority] = PublicKey.findProgramAddressSync(
      [Buffer.from("presale_vault")],
      program.programId,
    );
    presaleVault = await ata(openMint, presaleVaultAuthority, true);
    await mintOpen(presaleVault, openUnit(1_000_000));
  });

  describe("happy path + no-free-mint + binding", () => {
    const nonce = 900;
    let saleConfig: PublicKey;
    let usdcVault: PublicKey;
    let payer: Keypair;
    let payerUsdc: PublicKey;

    before(async () => {
      const treasury = await ata(usdcMint, admin.publicKey);
      const now = Math.floor(Date.now() / 1000);
      ({ saleConfig, usdcVault } = await initSale(
        nonce,
        {
          hardCap: usdcUnit(1_000_000),
          softCap: new BN(0),
          minContribution: usdcUnit(1),
          maxContribution: usdcUnit(1_000_000),
          maxSlippageBps: 100,
          openPerUsdc: new BN(100),
          startTime: new BN(now - 60),
          endTime: new BN(now + 3600),
          stablecoinWhitelist: [],
        },
        treasury,
      ));

      payer = Keypair.generate();
      await airdrop(payer.publicKey);
      payerUsdc = await ata(usdcMint, payer.publicKey);
      await mintUsdc(payerUsdc, usdcUnit(1_000));
    });

    it("delivers OPEN to recipient and records the contribution (happy path)", async () => {
      // `recipient` never signs, is never airdropped, and needs no prior
      // ATA — `payer` funds and creates everything on its behalf.
      const recipient = Keypair.generate().publicKey;
      const contribution = contributionPda(saleConfig, recipient);
      const recipientOpen = recipientOpenAta(recipient);

      const vaultBefore = await getAccount(connection, usdcVault, "confirmed", TOKEN_2022_PROGRAM_ID);

      await deliverBuilder({
        nonce,
        saleConfig,
        usdcVault,
        payer,
        sourceUsdc: payerUsdc,
        recipient,
        usdcAmount: usdcUnit(1),
      }).rpc({ commitment: "confirmed" });

      const vaultAfter = await getAccount(connection, usdcVault, "confirmed", TOKEN_2022_PROGRAM_ID);
      expect((vaultAfter.amount - vaultBefore.amount).toString()).to.equal(usdcUnit(1).toString());

      const acc = await program.account.contribution.fetch(contribution);
      expect(acc.amountUsdc.toString()).to.equal(usdcUnit(1).toString());
      // 1 USDC * openPerUsdc(100) = 100 OPEN, delivered (not merely
      // entitled) in this same instruction.
      expect(acc.openEntitlement.toString()).to.equal(openUnit(100).toString());
      expect(acc.claimedOpen.toString()).to.equal(openUnit(100).toString());

      const openAcc = await getAccount(connection, recipientOpen, "confirmed", TOKEN_2022_PROGRAM_ID);
      expect(openAcc.amount.toString()).to.equal(openUnit(100).toString());
      expect(openAcc.owner.toBase58()).to.equal(recipient.toBase58());
    });

    it("no free mint: a source with zero USDC reverts the whole tx, nothing recorded", async () => {
      const brokePayer = Keypair.generate();
      await airdrop(brokePayer.publicKey);
      const brokeSourceUsdc = await ata(usdcMint, brokePayer.publicKey); // created, never funded
      const recipient = Keypair.generate().publicKey;
      const contribution = contributionPda(saleConfig, recipient);

      const vaultBefore = await getAccount(connection, usdcVault, "confirmed", TOKEN_2022_PROGRAM_ID);

      await expectAnyRejection(
        deliverBuilder({
          nonce,
          saleConfig,
          usdcVault,
          payer: brokePayer,
          sourceUsdc: brokeSourceUsdc,
          recipient,
          usdcAmount: usdcUnit(1),
        }).rpc({ commitment: "confirmed" }),
      );

      const vaultAfter = await getAccount(connection, usdcVault, "confirmed", TOKEN_2022_PROGRAM_ID);
      expect(vaultAfter.amount.toString()).to.equal(vaultBefore.amount.toString());
      expect(await contributionExists(contribution)).to.equal(false);
    });

    it("binding: a payer cannot redirect the delivered OPEN to its own ATA", async () => {
      const recipient = Keypair.generate().publicKey;
      // Attacker-controlled ATA (owned by `payer`, not `recipient`) —
      // substituted in place of the correct `recipient_open`.
      const payerOpen = await ata(openMint, payer.publicKey);

      await expectAnyRejection(
        deliverBuilder({
          nonce,
          saleConfig,
          usdcVault,
          payer,
          sourceUsdc: payerUsdc,
          recipient,
          recipientOpen: payerOpen,
          usdcAmount: usdcUnit(1),
        }).rpc({ commitment: "confirmed" }),
      );

      // Nothing landed in the attacker's own OPEN account.
      const payerOpenAcc = await getAccount(connection, payerOpen, "confirmed", TOKEN_2022_PROGRAM_ID);
      expect(payerOpenAcc.amount.toString()).to.equal("0");
    });

    it("binding: recipient_account must match the bound `recipient` arg", async () => {
      const recipient = Keypair.generate().publicKey;
      const impostor = Keypair.generate().publicKey;

      await expectAnyRejection(
        deliverBuilder({
          nonce,
          saleConfig,
          usdcVault,
          payer,
          sourceUsdc: payerUsdc,
          recipient,
          recipientAccount: impostor, // wrong on purpose
          recipientOpen: recipientOpenAta(impostor),
          usdcAmount: usdcUnit(1),
        }).rpc({ commitment: "confirmed" }),
      );
    });
  });

  describe("caps", () => {
    const nonce = 901;
    let saleConfig: PublicKey;
    let usdcVault: PublicKey;
    let payer: Keypair;
    let payerUsdc: PublicKey;

    before(async () => {
      const treasury = await ata(usdcMint, admin.publicKey);
      const now = Math.floor(Date.now() / 1000);
      ({ saleConfig, usdcVault } = await initSale(
        nonce,
        {
          hardCap: usdcUnit(150),
          softCap: new BN(0),
          minContribution: usdcUnit(50),
          maxContribution: usdcUnit(100),
          maxSlippageBps: 100,
          openPerUsdc: new BN(100),
          startTime: new BN(now - 60),
          endTime: new BN(now + 3600),
          stablecoinWhitelist: [],
        },
        treasury,
      ));

      payer = Keypair.generate();
      await airdrop(payer.publicKey);
      payerUsdc = await ata(usdcMint, payer.publicKey);
      await mintUsdc(payerUsdc, usdcUnit(1_000));
    });

    it("rejects a first delivery below the minimum", async () => {
      const recipient = Keypair.generate().publicKey;
      await expectAnchorError(
        deliverBuilder({
          nonce,
          saleConfig,
          usdcVault,
          payer,
          sourceUsdc: payerUsdc,
          recipient,
          usdcAmount: usdcUnit(10),
        }).rpc({ commitment: "confirmed" }),
        "BelowMinimumContribution",
      );
    });

    it("delivers a valid contribution (60 USDC) for the cumulative-max spec below", async () => {
      const recipient = Keypair.generate().publicKey;
      await deliverBuilder({
        nonce,
        saleConfig,
        usdcVault,
        payer,
        sourceUsdc: payerUsdc,
        recipient,
        usdcAmount: usdcUnit(60),
      }).rpc({ commitment: "confirmed" });

      const contribution = contributionPda(saleConfig, recipient);
      const acc = await program.account.contribution.fetch(contribution);
      expect(acc.amountUsdc.toString()).to.equal(usdcUnit(60).toString());

      // A follow-up delivery to the SAME recipient that would exceed the
      // 100 USDC per-wallet maximum is rejected.
      await expectAnchorError(
        deliverBuilder({
          nonce,
          saleConfig,
          usdcVault,
          payer,
          sourceUsdc: payerUsdc,
          recipient,
          usdcAmount: usdcUnit(50),
        }).rpc({ commitment: "confirmed" }),
        "AboveMaximumContribution",
      );
    });

    it("rejects a delivery (to a second recipient) that would exceed the hard cap", async () => {
      // Sale already holds 60 USDC from the previous spec; hard cap is
      // 150 — a 100 USDC delivery would push total_raised to 160 > 150.
      const recipient = Keypair.generate().publicKey;
      await expectAnchorError(
        deliverBuilder({
          nonce,
          saleConfig,
          usdcVault,
          payer,
          sourceUsdc: payerUsdc,
          recipient,
          usdcAmount: usdcUnit(100),
        }).rpc({ commitment: "confirmed" }),
        "HardCapExceeded",
      );
    });
  });

  describe("ban gate", () => {
    it("refuses delivery to a banned recipient (OFS-7100 §12)", async () => {
      await getSharedGovernanceConfig(governance);
      const nonce = 902;
      const treasury = await ata(usdcMint, admin.publicKey);
      const now = Math.floor(Date.now() / 1000);
      const { saleConfig, usdcVault } = await initSale(
        nonce,
        {
          hardCap: usdcUnit(1_000),
          softCap: new BN(0),
          minContribution: usdcUnit(1),
          maxContribution: usdcUnit(1_000),
          maxSlippageBps: 100,
          openPerUsdc: new BN(100),
          startTime: new BN(now - 60),
          endTime: new BN(now + 3600),
          stablecoinWhitelist: [],
        },
        treasury,
      );

      const payer = Keypair.generate();
      await airdrop(payer.publicKey);
      const payerUsdc = await ata(usdcMint, payer.publicKey);
      await mintUsdc(payerUsdc, usdcUnit(1_000));

      // The recipient is banned, even though the payer (deBridge executor)
      // is not — the gate reaches whoever is meant to receive the funds.
      const bannedRecipient = Keypair.generate().publicKey;
      await banWallet(governance, staking, bannedRecipient, { sanctions: {} }, [
        ...Buffer.alloc(32, 9),
      ]);

      await expectAnchorError(
        deliverBuilder({
          nonce,
          saleConfig,
          usdcVault,
          payer,
          sourceUsdc: payerUsdc,
          recipient: bannedRecipient,
          usdcAmount: usdcUnit(10),
        }).rpc({ commitment: "confirmed" }),
        "WalletBanned",
      );
    });
  });
});
