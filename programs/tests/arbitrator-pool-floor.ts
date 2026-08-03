// OFS-4100 Annex A, option A — a case must not open a round the eligible
// arbitrator pool cannot staff, and when it stops for that reason it must
// say so.
//
// # Why this is its own spec file
//
// It writes a *global singleton* — the `ArbitrationPolicy` — that changes
// how every dispute in the run resolves. `tests/escrow.ts` deliberately
// leaves the arbitrator-eligibility parameters at their shipped zeros so its
// specs exercise the ordinary path; a pool figure left standing there would
// silently end its multi-round cases early and the failures would surface in
// a file that never mentioned the parameter. Keeping the perturbation here,
// with an `after` that puts the singleton back to zero, keeps the blast
// radius to this file.
//
// # What is actually being proven
//
// Barring a silent seat-holder retires up to a full bench per round, and the
// round that ends a case still needs `MIN_ARBITRATORS` counted reveals — so a
// case is only decidable on a pool of at least
// `MIN_ARBITRATORS + MAX_BARRED_ARBITRATORS` = 17. Below that the case cannot
// be decided however honestly everyone behaves, and lands on the terminal
// even split, which is exactly what the party facing a losing verdict wanted.
//
// The cheapest honest instance of that is a pool below the quorum floor
// itself: with a published pool of one, no round of any case can ever reach
// three counted reveals. The case must therefore stop at the end of its first
// round with `PoolExhausted` recorded, instead of bouncing to its round
// budget and splitting anyway with no record of why. Both paths pay out
// identically — the whole point of the change is the record and the timing,
// not the money — so the assertions below check the split *and* the reason.
import * as anchor from "@anchor-lang/core";
import { Program, BN } from "@anchor-lang/core";
import { Escrow } from "../target/types/escrow";
import {
  TOKEN_2022_PROGRAM_ID,
  mintTo,
  getOrCreateAssociatedTokenAccount,
  getAccount,
} from "@solana/spl-token";
import { Keypair, PublicKey, SystemProgram, SYSVAR_RENT_PUBKEY } from "@solana/web3.js";
import { expect } from "chai";
import {
  getSharedMint,
  getSharedOpenMint,
  getSharedArbitrationPool,
  getSharedFeeConfig,
  unit,
} from "./shared-fixtures";

describe("arbitrator pool floor (OFS-4100 Annex A)", () => {
  anchor.setProvider(anchor.AnchorProvider.env());
  const provider = anchor.AnchorProvider.env();
  const connection = provider.connection;
  const program = anchor.workspace.escrow as Program<Escrow>;
  const admin = (provider.wallet as anchor.Wallet).payer;

  /** `escrow::state::MIN_ARBITRATORS`. */
  const MIN_ARBITRATORS = 3;

  let mint: PublicKey;
  let openMint: PublicKey;
  let feeConfig: PublicKey;
  let devTreasury: PublicKey;
  let ecosystemTreasury: PublicKey;
  let infraTreasury: PublicKey;
  let emergencyReserve: PublicKey;
  let arbitrationPool: PublicKey;

  const arbitrationPolicy = PublicKey.findProgramAddressSync(
    [Buffer.from("arbitration_policy")],
    program.programId
  )[0];

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
      TOKEN_2022_PROGRAM_ID
    );
    return acc.address;
  }

  /** Same local-validator blockhash race the other suites retry on. */
  async function withBlockhashRetry<T>(fn: () => Promise<T>, attempts = 4): Promise<T> {
    for (let i = 0; i < attempts; i++) {
      try {
        return await fn();
      } catch (err) {
        const race = err instanceof Error && err.message.includes("Blockhash not found");
        if (!race || i === attempts - 1) throw err;
        await new Promise((r) => setTimeout(r, 250));
      }
    }
    throw new Error("unreachable");
  }

  /**
   * Asserts an instruction is rejected with a specific Anchor error code.
   *
   * A bare `try { …; expect.fail() } catch` cannot do this: `expect.fail`
   * throws, its own `AssertionError` is caught by the same `catch`, and the
   * spec then reports a confusing code mismatch instead of "it succeeded when
   * it should not have". Worse in the other direction — any incidental error,
   * a blockhash race included, reads as the rejection under test. So the send
   * is retried like every other one here, and the success case is recorded
   * rather than thrown.
   */
  async function expectAnchorError(send: () => Promise<unknown>, code: string) {
    let succeeded = false;
    try {
      await withBlockhashRetry(send);
      succeeded = true;
    } catch (err: any) {
      expect(err?.error?.errorCode?.code ?? String(err)).to.equal(code);
    }
    expect(succeeded, `expected rejection with ${code}, but the call succeeded`).to.equal(false);
  }

  const pda = (seed: string, ...parts: Buffer[]) =>
    PublicKey.findProgramAddressSync([Buffer.from(seed), ...parts], program.programId)[0];
  const idBytes = (id: number) => new BN(id).toArrayLike(Buffer, "le", 8);

  const liquidityVaultPda = (merchant: PublicKey, m: PublicKey) =>
    pda("liquidity_vault", merchant.toBuffer(), m.toBuffer());
  const liquidityTokenVaultPda = (merchant: PublicKey, m: PublicKey) =>
    pda("liquidity_vault_tokens", merchant.toBuffer(), m.toBuffer());
  const tradeEscrowPda = (id: number) => pda("trade_escrow", idBytes(id));
  const tradeEscrowTokenVaultPda = (id: number) => pda("trade_escrow_tokens", idBytes(id));
  const disputeCasePda = (id: number) => pda("dispute_case", idBytes(id));

  /** Creates a merchant, funds and freezes a trade escrow, opens a case. */
  async function openDisputedTrade(reservationId: number, amount: BN) {
    const merchant = Keypair.generate();
    const buyer = Keypair.generate();
    await airdrop(merchant.publicKey);
    await airdrop(buyer.publicKey);

    const liquidityVault = liquidityVaultPda(merchant.publicKey, mint);
    const liquidityTokenVault = liquidityTokenVaultPda(merchant.publicKey, mint);
    const tradeEscrow = tradeEscrowPda(reservationId);
    const tradeEscrowTokenVault = tradeEscrowTokenVaultPda(reservationId);

    for (const [m, vault, tokenVault] of [
      [mint, liquidityVault, liquidityTokenVault],
      [
        openMint,
        liquidityVaultPda(merchant.publicKey, openMint),
        liquidityTokenVaultPda(merchant.publicKey, openMint),
      ],
    ] as [PublicKey, PublicKey, PublicKey][]) {
      await withBlockhashRetry(() =>
        program.methods
          .createLiquidityVault()
          .accountsPartial({
            merchant: merchant.publicKey,
            mint: m,
            liquidityVault: vault,
            tokenVault,
            tokenProgram: TOKEN_2022_PROGRAM_ID,
            systemProgram: SystemProgram.programId,
            rent: SYSVAR_RENT_PUBKEY,
          })
          .signers([merchant])
          .rpc({ commitment: "confirmed" })
      );
    }

    const merchantAta = await ata(mint, merchant.publicKey);
    await mintTo(
      connection,
      admin,
      mint,
      merchantAta,
      admin,
      BigInt(amount.muln(2).toString()),
      [],
      { commitment: "confirmed" },
      TOKEN_2022_PROGRAM_ID
    );

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
        .rpc({ commitment: "confirmed" })
    );
    await withBlockhashRetry(() =>
      program.methods
        .reserveLiquidity(amount)
        .accountsPartial({ merchant: merchant.publicKey, liquidityVault })
        .signers([merchant])
        .rpc({ commitment: "confirmed" })
    );
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
        .rpc({ commitment: "confirmed" })
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
        .rpc({ commitment: "confirmed" })
    );

    // The protocol minimum window on both halves, so the case can be driven
    // past its reveal deadline in one wait rather than three.
    await withBlockhashRetry(() =>
      program.methods
        .openDisputeCase(new BN(60), new BN(60))
        .accountsPartial({
          signer: buyer.publicKey,
          payer: admin.publicKey,
          tradeEscrow,
          disputeCase: disputeCasePda(reservationId),
          feeConfig,
          depositMint: openMint,
          merchantOpenVault: liquidityVaultPda(merchant.publicKey, openMint),
          merchantOpenTokenVault: liquidityTokenVaultPda(merchant.publicKey, openMint),
          arbitrationPool,
          tokenProgram: TOKEN_2022_PROGRAM_ID,
          systemProgram: SystemProgram.programId,
        })
        .signers([buyer])
        .rpc({ commitment: "confirmed" })
    );

    return {
      merchant,
      buyer,
      liquidityVault,
      tradeEscrow,
      tradeEscrowTokenVault,
    };
  }

  /**
   * `execute_dispute_outcome`, with the arbitration policy passed as the
   * optional trailing account.
   *
   * It is a `remainingAccounts` entry rather than a named one because
   * `ExecuteDisputeOutcome`'s generated `try_accounts` is already close to
   * SBF's stack-frame limit, and because the account has to stay optional —
   * every existing cluster resolves disputes without it. Omitting it is what
   * `resolvesWithoutThePolicy` below exercises.
   */
  function executeOutcome(
    reservationId: number,
    merchant: PublicKey,
    buyerAta: PublicKey,
    withPolicy: boolean
  ) {
    const call = program.methods.executeDisputeOutcome().accountsPartial({
      mint,
      disputeCase: disputeCasePda(reservationId),
      tradeEscrow: tradeEscrowPda(reservationId),
      tradeEscrowTokenVault: tradeEscrowTokenVaultPda(reservationId),
      liquidityVault: liquidityVaultPda(merchant, mint),
      liquidityTokenVault: liquidityTokenVaultPda(merchant, mint),
      buyerTokenAccount: buyerAta,
      feeConfig,
      devTreasury,
      ecosystemTreasury,
      infraTreasury,
      emergencyReserve,
      depositMint: openMint,
      arbitrationPool,
      merchantOpenVault: liquidityVaultPda(merchant, openMint),
      merchantOpenTokenVault: liquidityTokenVaultPda(merchant, openMint),
      tokenProgram: TOKEN_2022_PROGRAM_ID,
      depositTokenProgram: TOKEN_2022_PROGRAM_ID,
    });
    const withAccounts = withPolicy
      ? call.remainingAccounts([{ pubkey: arbitrationPolicy, isWritable: false, isSigner: false }])
      : call;
    return withBlockhashRetry(() => withAccounts.rpc({ commitment: "confirmed" }));
  }

  function publishPoolSize(eligibleArbitrators: number) {
    return withBlockhashRetry(() =>
      program.methods
        .publishArbitratorPoolSize(eligibleArbitrators)
        .accountsPartial({
          admin: admin.publicKey,
          feeConfig,
          arbitrationPolicy,
          systemProgram: SystemProgram.programId,
        })
        .rpc({ commitment: "confirmed" })
    );
  }

  before(async () => {
    mint = await getSharedMint();
    openMint = await getSharedOpenMint();
    ({ feeConfig, devTreasury, ecosystemTreasury, infraTreasury, emergencyReserve } =
      await getSharedFeeConfig(program));
    arbitrationPool = await getSharedArbitrationPool(program);
  });

  after(async () => {
    // Back to unpublished. The policy is a singleton every later dispute in
    // the run would otherwise be measured against.
    await publishPoolSize(0);
  });

  it("publishes the pool size and reports it against the floor the constants derive", async () => {
    await publishPoolSize(9);
    const policy = await program.account.arbitrationPolicy.fetch(arbitrationPolicy);
    expect(policy.eligibleArbitrators).to.equal(9);
    expect(policy.admin.toBase58()).to.equal(admin.publicKey.toBase58());
    expect(policy.updatedAt.toNumber()).to.be.greaterThan(0);
  });

  it("only the fee-config admin may publish a pool size", async () => {
    const impostor = Keypair.generate();
    await airdrop(impostor.publicKey, 1);
    // The pool size can end live cases early, so the wrong hand on it is not
    // a cosmetic problem — it is a way to hand a griefing party the terminal
    // split without ever taking a seat.
    await expectAnchorError(
      () =>
        program.methods
          .publishArbitratorPoolSize(17)
          .accountsPartial({
            admin: impostor.publicKey,
            feeConfig,
            arbitrationPolicy,
            systemProgram: SystemProgram.programId,
          })
          .signers([impostor])
          .rpc({ commitment: "confirmed" }),
      "Unauthorized"
    );
  });

  it("stops a case the published pool cannot staff, and records that as the reason", async () => {
    const reservationId = 9310;
    const amount = unit(400);

    // One eligible arbitrator: below `MIN_ARBITRATORS`, so no round of this
    // case — or any other — could ever reach a counted quorum. The floor for
    // the next round is `MIN_ARBITRATORS + 0 barred` = 3.
    await publishPoolSize(1);

    const { merchant, buyer, liquidityVault } = await openDisputedTrade(reservationId, amount);
    const buyerAta = await ata(mint, buyer.publicKey);
    const buyerBefore = await getAccount(connection, buyerAta, "confirmed", TOKEN_2022_PROGRAM_ID);
    const vaultBefore = await program.account.liquidityVault.fetch(liquidityVault);

    // Past both windows with nobody having committed.
    await new Promise((r) => setTimeout(r, 121_000));
    await executeOutcome(reservationId, merchant.publicKey, buyerAta, true);

    const caseAccount = await program.account.disputeCase.fetch(disputeCasePda(reservationId));

    // The heart of it. Without the pool floor this round simply re-opens:
    // `resolved` stays false and `round` advances to 1, and the case grinds
    // through its whole budget before splitting with nothing on the account
    // to say the pool was never going to staff it.
    expect(caseAccount.resolved, "the case must resolve rather than re-open").to.equal(true);
    expect(caseAccount.round, "it must stop on the round it was already on").to.equal(0);
    expect(caseAccount.outcome, "no verdict was reached").to.equal(null);
    expect(
      caseAccount.terminalReason,
      "the split must be recorded as pool exhaustion, not as disagreement"
    ).to.deep.equal({ poolExhausted: {} });

    // ...and the payout is the ordinary terminal split, unchanged. The point
    // of the floor is when it happens and that it is recorded, not what it
    // pays — a floor that moved money would be a new economic parameter
    // rather than an observability fix.
    const half = amount.div(new BN(2));
    const buyerAfter = await getAccount(connection, buyerAta, "confirmed", TOKEN_2022_PROGRAM_ID);
    expect((buyerAfter.amount - buyerBefore.amount).toString()).to.equal(half.toString());
    const vaultAfter = await program.account.liquidityVault.fetch(liquidityVault);
    expect(vaultAfter.available.toString()).to.equal(
      vaultBefore.available.add(amount.sub(half)).toString()
    );
  });

  it("re-opens as before once the published pool clears the floor", async () => {
    const reservationId = 9311;
    const amount = unit(400);

    // Exactly `MIN_ARBITRATORS`, which is the floor for the first re-opening
    // and therefore the boundary case: one fewer stops the case, this many
    // must not. A floor that refused here would be ending cases that are
    // still decidable, which is the failure mode worth guarding hardest
    // against — it hands the griefing party their split sooner and for free.
    await publishPoolSize(MIN_ARBITRATORS);

    const { merchant, buyer } = await openDisputedTrade(reservationId, amount);
    const buyerAta = await ata(mint, buyer.publicKey);

    await new Promise((r) => setTimeout(r, 121_000));
    await executeOutcome(reservationId, merchant.publicKey, buyerAta, true);

    const caseAccount = await program.account.disputeCase.fetch(disputeCasePda(reservationId));
    expect(caseAccount.resolved, "a staffable pool must still get its rounds").to.equal(false);
    expect(caseAccount.round).to.equal(1);
    expect(caseAccount.terminalReason).to.equal(null);
  });

  it("leaves the floor unenforced when the policy account is not supplied", async () => {
    const reservationId = 9312;
    const amount = unit(400);

    // A pool of one again — but the caller omits the policy account, which is
    // every caller on every cluster today. The case must behave exactly as it
    // did before this change rather than failing to execute, because
    // `execute_dispute_outcome` is permissionless and cannot be made to
    // depend on an account no deployment has yet created.
    await publishPoolSize(1);

    const { merchant, buyer } = await openDisputedTrade(reservationId, amount);
    const buyerAta = await ata(mint, buyer.publicKey);

    await new Promise((r) => setTimeout(r, 121_000));
    await executeOutcome(reservationId, merchant.publicKey, buyerAta, false);

    const caseAccount = await program.account.disputeCase.fetch(disputeCasePda(reservationId));
    expect(caseAccount.resolved).to.equal(false);
    expect(caseAccount.round).to.equal(1);
    expect(caseAccount.terminalReason).to.equal(null);
  });
});
