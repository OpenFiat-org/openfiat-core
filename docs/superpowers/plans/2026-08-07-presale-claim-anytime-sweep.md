# Presale claim-anytime + sweep_proceeds — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let presale buyers claim their OPEN any time during the live sale, and let the (multisig) admin sweep USDC proceeds to the fixed treasury while the sale runs — made sound by removing the refund path.

**Architecture:** Anchor program `programs/programs/presale`. Three coordinated changes: (1) simplify the sale state machine — remove `refund`/`SoftCapMissed`, force `soft_cap == 0`, make `finalize_sale` always resolve to `Finalized`; (2) ungate `claim` from finalization using a `claimed_open` high-water mark so repeat contribute→claim cycles pay only the newly-accrued delta; (3) add a `sweep_proceeds` instruction whose USDC destination is hard-constrained to `sale_config.treasury`. Then re-prove the full lifecycle on devnet.

**Tech Stack:** Rust 1.89.0, Anchor CLI 1.1.2, `anchor_spl::token_interface` (Token-2022-aware), TypeScript tests via `ts-mocha` (`@anchor-lang/core`, `@solana/spl-token`).

## Global Constraints

- **Devnet only.** No mainnet deploy under this plan. The string `mainnet-beta` must never appear anywhere in `programs/` — `programs-ci.yml` fails the build if it does.
- **Token facts (do not change):** OPEN total supply 1,000,000,000 at 9 decimals, mint + freeze authority permanently revoked. Presale sells the 200,000,000-OPEN Community Presale bucket at 1 OPEN = 1 USDC.
- **`soft_cap` MUST be 0** on every sale after this change (no refund path exists). `max_contribution` = 10,000,000 USDC-equivalent.
- **Error codes are append-only.** `error.rs` documents that Anchor numbers codes by declaration order; removing/reordering a variant renumbers later codes and breaks clients matching on the number. Retain now-unused variants (`SaleNotFinalized`, `SaleNotRefundable`, `AlreadyRefunded`, `AlreadyClaimed`); append new variants at the end only.
- **CPI pattern (this fork):** `transfer_checked` takes `CpiContext::new[_with_signer](ctx.accounts.token_program.key(), TransferChecked { .. })` — the program is passed by **`.key()`**, matching every existing instruction. Do not switch to `to_account_info()`.
- **Commits:** conventional-commit messages, no Claude attribution/co-author trailer. After each task, commit AND push to `origin/main`.
- **Build/test command:** from `programs/`, run `anchor test --validator legacy` (spins an ephemeral `solana-test-validator`, builds all programs, runs the full `tests/**/*.ts` suite). To focus one file while iterating you may append `-- --grep "<describe or it text>"`, but the canonical green check is the full run.
- **Fresh-deploy semantics:** account layouts may change; there is no mainnet state to migrate, and devnet is re-initialized under a **new `sale_nonce`** (existing devnet `SaleConfig`/`Contribution` accounts at old nonces are disposable test state).

---

### Task 1: Simplify the sale state machine (remove refund + SoftCapMissed, force soft_cap=0, finalize always Finalized)

Establishes a sound base: no refund path can coexist with the later claim-anytime change. Leaves the program compiling with `claim` still finalize-gated (that gate is removed in Task 2).

**Files:**
- Modify: `programs/programs/presale/src/state.rs` (remove `SoftCapMissed` from `SaleState`)
- Modify: `programs/programs/presale/src/error.rs` (append `SoftCapNotSupported`)
- Modify: `programs/programs/presale/src/instructions/initialize_sale.rs` (guard `soft_cap == 0`)
- Modify: `programs/programs/presale/src/instructions/update_sale_params.rs` (guard `soft_cap == 0`)
- Modify: `programs/programs/presale/src/instructions/finalize_sale.rs` (always sweep + `Finalized`)
- Delete: `programs/programs/presale/src/instructions/refund.rs`
- Modify: `programs/programs/presale/src/instructions.rs` (drop `refund` module)
- Modify: `programs/programs/presale/src/lib.rs` (drop `refund` entry point)
- Test: `programs/tests/presale.ts`

**Interfaces:**
- Produces: `SaleState` with variants `{ Active, Finalized }` only. `ErrorCode::SoftCapNotSupported` appended after `WalletBanned`. `initialize_sale`/`update_sale_params` reject `soft_cap != 0`. `finalize_sale` always → `Finalized`. No `refund` instruction in the IDL.

- [ ] **Step 1: Append the new error variant**

In `error.rs`, add at the very end of the enum (after `WalletBanned`), preserving the append-only rule:

```rust
    #[msg("soft_cap must be zero: this sale has no refund path, so a non-zero soft cap is unsupported")]
    SoftCapNotSupported,
```

- [ ] **Step 2: Remove `SoftCapMissed` from `SaleState`**

In `state.rs`, change the enum and its doc comment:

```rust
/// Lifecycle state of the presale (OFS-4200 §3).
///
/// `Active -> Finalized` is the only transition out of `Active` and is
/// terminal. There is no refund path (soft_cap is forced to 0 at
/// initialization), so there is no `SoftCapMissed` state.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace, Debug)]
pub enum SaleState {
    Active,
    Finalized,
}
```

- [ ] **Step 3: Guard `soft_cap == 0` at initialization**

In `initialize_sale.rs`, immediately after the existing `hard_cap > soft_cap` require, add:

```rust
    require!(params.soft_cap == 0, ErrorCode::SoftCapNotSupported);
```

- [ ] **Step 4: Guard `soft_cap == 0` on update**

In `update_sale_params.rs`, immediately after the existing `hard_cap > soft_cap` require, add the same line:

```rust
    require!(params.soft_cap == 0, ErrorCode::SoftCapNotSupported);
```

- [ ] **Step 5: Make `finalize_sale` always resolve to `Finalized`**

Replace the body of `handle_finalize_sale` (the `soft_cap_met` branch and the conditional state set) with an unconditional sweep + finalize:

```rust
pub fn handle_finalize_sale(ctx: Context<FinalizeSale>, sale_nonce: u64) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let sale_config = &ctx.accounts.sale_config;

    require!(
        sale_config.state == SaleState::Active,
        ErrorCode::SaleAlreadyResolved
    );
    require!(
        now > sale_config.end_time || sale_config.total_raised >= sale_config.hard_cap,
        ErrorCode::SaleNotEnded
    );

    let bump = sale_config.bump;
    let usdc_decimals = sale_config.usdc_decimals;
    let nonce_bytes = sale_nonce.to_le_bytes();
    let signer_seeds: &[&[u8]] = &[SALE_CONFIG_SEED, &nonce_bytes, &[bump]];

    let amount = ctx.accounts.usdc_vault.amount;
    if amount > 0 {
        transfer_checked(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.key(),
                TransferChecked {
                    from: ctx.accounts.usdc_vault.to_account_info(),
                    mint: ctx.accounts.usdc_mint.to_account_info(),
                    to: ctx.accounts.treasury.to_account_info(),
                    authority: ctx.accounts.sale_config.to_account_info(),
                },
                &[signer_seeds],
            ),
            amount,
            usdc_decimals,
        )?;
    }

    ctx.accounts.sale_config.state = SaleState::Finalized;
    Ok(())
}
```

- [ ] **Step 6: Delete the refund instruction and its wiring**

Delete the file `programs/programs/presale/src/instructions/refund.rs`. In `instructions.rs` remove both `pub mod refund;` and `pub use refund::*;`. In `lib.rs` remove the entire `pub fn refund(...) { .. }` entry point.

- [ ] **Step 7: Update existing tests — soft caps to zero, drop refund/soft-cap-missed cases**

In `programs/tests/presale.ts`:
- In the `initSale` helper and **every** test's params object, set `softCap: new BN(0)` (replace each `softCap: usdcUnit(N)`). The `hard_cap > soft_cap` invariant still holds since hard caps are positive.
- In the `initialize_sale` describe block, add a case asserting a non-zero soft cap is rejected:

```typescript
it("rejects a sale with a non-zero soft cap", async () => {
  const nonce = nextNonce();
  await expectAnchorError(
    initSale(nonce, {
      hardCap: usdcUnit(1_000_000),
      softCap: usdcUnit(1),
      minContribution: usdcUnit(1),
      maxContribution: usdcUnit(1_000_000),
      maxSlippageBps: 100,
      startOffset: -5,
      endOffset: 3600,
      whitelist: [],
    }),
    "SoftCapNotSupported",
  );
});
```

- In the `update_sale_params` describe block, change the existing "rejects a hard cap that isn't greater than the soft cap" test so its params use `softCap: new BN(0)` and a valid `hardCap`, and instead assert it now rejects a non-zero soft cap with `"SoftCapNotSupported"` (a hard-cap-≤-soft-cap case is no longer reachable with soft_cap pinned to 0). Keep the non-admin-rejection and raise-cap tests, with `softCap: new BN(0)`.
- Delete any `describe`/`it` blocks that exercise `refund`, `SoftCapMissed`, or a finalize that resolves to soft-cap-missed. Delete helper calls to `program.methods.refund(...)`.
- In any finalize test, assert the resolved state is `Finalized` and the treasury received the full raised amount.

- [ ] **Step 8: Run the suite to verify green**

Run (from `programs/`): `anchor test --validator legacy`
Expected: PASS. No test references `refund` or `SoftCapMissed`; the new `SoftCapNotSupported` cases pass; finalize tests assert `Finalized`.

- [ ] **Step 9: Commit and push**

```bash
cd /OpenFiat/openfiat-core
git add programs/programs/presale/src programs/tests/presale.ts
git commit -m "feat(presale): remove refund path, force soft_cap=0, finalize always resolves Finalized"
git push origin main
```

---

### Task 2: Claim-anytime via a `claimed_open` high-water mark

Buyers claim accrued OPEN during the live sale; repeat claims after further contributions pay only the delta.

**Files:**
- Modify: `programs/programs/presale/src/state.rs` (`Contribution`: `claimed: bool` + `refunded: bool` → `claimed_open: u64`)
- Modify: `programs/programs/presale/src/error.rs` (append `NothingToClaim`)
- Modify: `programs/programs/presale/src/instructions/claim.rs` (ungate; pay delta)
- Test: `programs/tests/presale.ts`

**Interfaces:**
- Consumes: `SaleState` from Task 1.
- Produces: `Contribution { buyer, amount_usdc, open_entitlement, claimed_open, bump }`. `ErrorCode::NothingToClaim` appended after `SoftCapNotSupported`. `claim` transfers `open_entitlement - claimed_open` and requires the sale not be a state it can never reach — no finalize gate.

- [ ] **Step 1: Append the new error variant**

In `error.rs`, after `SoftCapNotSupported` (still the last variant), append:

```rust
    #[msg("Nothing to claim: your full OPEN entitlement has already been claimed")]
    NothingToClaim,
```

- [ ] **Step 2: Change the `Contribution` account layout**

In `state.rs`, replace the `claimed`/`refunded` fields:

```rust
/// Per-buyer contribution record: PDA seeds `[CONTRIBUTION_SEED, sale_config, buyer]`.
#[account]
#[derive(InitSpace)]
pub struct Contribution {
    pub buyer: Pubkey,
    /// Cumulative USDC-equivalent contributed by this wallet, base units.
    pub amount_usdc: u64,
    /// OPEN base units this wallet is entitled to (1:1 with amount_usdc at
    /// the mint's decimals — OFS-4100 §3 confirms no presale vesting).
    pub open_entitlement: u64,
    /// OPEN base units already claimed. Monotonic high-water mark: a claim
    /// pays `open_entitlement - claimed_open`, so a buyer who contributes
    /// again after claiming can claim only the newly-accrued delta.
    pub claimed_open: u64,
    pub bump: u8,
}
```

- [ ] **Step 3: Rewrite the `claim` handler to pay the unclaimed delta with no finalize gate**

In `claim.rs`, replace `handle_claim` (leave the `Claim` accounts struct unchanged — it still needs `open_mint`, `presale_vault_authority`, `presale_vault`, `buyer_open`, `token_program`):

```rust
pub fn handle_claim(ctx: Context<Claim>, _sale_nonce: u64) -> Result<()> {
    // No finalize gate: OPEN is claimable while the sale is Active or
    // Finalized. Soundness rests on the oversell invariant — total
    // entitlements are capped at hard_cap and presale_vault holds exactly
    // that much OPEN — plus the monotonic high-water mark below.
    let contribution = &ctx.accounts.contribution;
    let unclaimed = contribution
        .open_entitlement
        .checked_sub(contribution.claimed_open)
        .ok_or(ErrorCode::Overflow)?;
    require!(unclaimed > 0, ErrorCode::NothingToClaim);

    let bump = ctx.bumps.presale_vault_authority;
    let signer_seeds: &[&[u8]] = &[PRESALE_VAULT_SEED, &[bump]];
    let open_decimals = ctx.accounts.sale_config.open_decimals;

    transfer_checked(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: ctx.accounts.presale_vault.to_account_info(),
                mint: ctx.accounts.open_mint.to_account_info(),
                to: ctx.accounts.buyer_open.to_account_info(),
                authority: ctx.accounts.presale_vault_authority.to_account_info(),
            },
            &[signer_seeds],
        ),
        unclaimed,
        open_decimals,
    )?;

    ctx.accounts.contribution.claimed_open = ctx.accounts.contribution.open_entitlement;
    Ok(())
}
```

- [ ] **Step 4: Add claim-anytime tests**

In `programs/tests/presale.ts`, add a describe block (reuse the file's `initSale`, `ata`, `mintTo9or6`, `usdcUnit`, `openUnit`, `getAccount`, `expectAnchorError` helpers; a sale started with `startOffset: -5` is immediately Active):

```typescript
describe("claim (anytime)", () => {
  it("delivers OPEN during the live sale and pays only the newly-accrued delta on re-claim", async () => {
    const nonce = nextNonce();
    await initSale(nonce, {
      hardCap: usdcUnit(1_000_000),
      softCap: new BN(0),
      minContribution: usdcUnit(1),
      maxContribution: usdcUnit(1_000_000),
      maxSlippageBps: 100,
      startOffset: -5,
      endOffset: 3600,
      whitelist: [],
    });

    const buyer = Keypair.generate();
    await airdrop(buyer.publicKey);
    const buyerUsdc = await ata(usdcMint, buyer.publicKey);
    const buyerOpen = await ata(openMint, buyer.publicKey);
    await mintTo9or6(usdcMint, buyerUsdc, usdcUnit(1_000));

    // First contribution + immediate claim while Active.
    await contributeUsdc(nonce, buyer, buyerUsdc, usdcUnit(60));
    await claim(nonce, buyer, buyerOpen);
    expect((await getAccount(connection, buyerOpen, "confirmed", TOKEN_2022_PROGRAM_ID)).amount.toString())
      .to.equal(openUnit(60).toString());

    // Re-claim with nothing new accrued → NothingToClaim.
    await expectAnchorError(claim(nonce, buyer, buyerOpen), "NothingToClaim");

    // Contribute more, then claim only the delta (40 more OPEN, total 100).
    await contributeUsdc(nonce, buyer, buyerUsdc, usdcUnit(40));
    await claim(nonce, buyer, buyerOpen);
    expect((await getAccount(connection, buyerOpen, "confirmed", TOKEN_2022_PROGRAM_ID)).amount.toString())
      .to.equal(openUnit(100).toString());
  });
});
```

If `contributeUsdc(...)` / `claim(...)` thin wrappers do not already exist in the file, add them next to `initSale` mirroring the existing `.methods.contributeUsdc(...)` / `.methods.claim(...)` call sites (pass `sale_config`, `contribution`, vault, mint, `presale_vault_authority`, and `token_program: TOKEN_2022_PROGRAM_ID` accounts exactly as the current inline tests do).

- [ ] **Step 5: Run the suite to verify green**

Run (from `programs/`): `anchor test --validator legacy`
Expected: PASS, including the new "claim (anytime)" block.

- [ ] **Step 6: Commit and push**

```bash
cd /OpenFiat/openfiat-core
git add programs/programs/presale/src programs/tests/presale.ts
git commit -m "feat(presale): claim OPEN anytime via claimed_open high-water mark"
git push origin main
```

---

### Task 3: `sweep_proceeds` instruction + `ProceedsSwept` event

Admin (multisig, on mainnet) draws down USDC to the fixed treasury while the sale runs.

**Files:**
- Create: `programs/programs/presale/src/instructions/sweep_proceeds.rs`
- Modify: `programs/programs/presale/src/instructions.rs` (register module)
- Modify: `programs/programs/presale/src/error.rs` (append `InvalidSweepAmount`)
- Modify: `programs/programs/presale/src/lib.rs` (add entry point)
- Test: `programs/tests/presale.ts`

**Interfaces:**
- Consumes: `SaleState`, `SaleConfig`, `SALE_CONFIG_SEED` from earlier tasks.
- Produces: instruction `sweep_proceeds(sale_nonce: u64, amount: u64)`; event `ProceedsSwept { sale_config, treasury, amount, vault_remaining }`; `ErrorCode::InvalidSweepAmount` appended after `NothingToClaim`.

- [ ] **Step 1: Append the new error variant**

In `error.rs`, after `NothingToClaim` (now last), append:

```rust
    #[msg("sweep amount must be greater than zero and at most the vault balance")]
    InvalidSweepAmount,
```

- [ ] **Step 2: Create the `sweep_proceeds` instruction**

Create `programs/programs/presale/src/instructions/sweep_proceeds.rs`:

```rust
use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    transfer_checked, Mint, TokenAccount, TokenInterface, TransferChecked,
};

use crate::{constants::*, error::ErrorCode, state::*};

/// Emitted on every successful sweep so contributors can watch proceeds
/// move to the published treasury on-chain.
#[event]
pub struct ProceedsSwept {
    pub sale_config: Pubkey,
    pub treasury: Pubkey,
    pub amount: u64,
    pub vault_remaining: u64,
}

/// Admin-gated draw-down of collected USDC to the sale's fixed treasury
/// while the sale is still Active. The destination is constrained to
/// `sale_config.treasury`: the admin controls *when* to sweep, never
/// *where* the funds go. Sound only because there is no refund path — see
/// the claim-anytime / soft_cap=0 design (OFS-4100 §3).
#[derive(Accounts)]
#[instruction(sale_nonce: u64)]
pub struct SweepProceeds<'info> {
    pub admin: Signer<'info>,

    #[account(
        mut,
        seeds = [SALE_CONFIG_SEED, &sale_nonce.to_le_bytes()],
        bump = sale_config.bump,
        has_one = admin @ ErrorCode::Unauthorized,
        constraint = sale_config.usdc_vault == usdc_vault.key(),
        constraint = sale_config.treasury == treasury.key(),
        constraint = sale_config.usdc_mint == usdc_mint.key(),
    )]
    pub sale_config: Account<'info, SaleConfig>,

    #[account(mut)]
    pub usdc_vault: InterfaceAccount<'info, TokenAccount>,

    #[account(mut)]
    pub treasury: InterfaceAccount<'info, TokenAccount>,

    #[account(mint::token_program = token_program)]
    pub usdc_mint: InterfaceAccount<'info, Mint>,

    pub token_program: Interface<'info, TokenInterface>,
}

pub fn handle_sweep_proceeds(
    ctx: Context<SweepProceeds>,
    sale_nonce: u64,
    amount: u64,
) -> Result<()> {
    require!(
        ctx.accounts.sale_config.state == SaleState::Active,
        ErrorCode::SaleAlreadyResolved
    );

    let vault_amount = ctx.accounts.usdc_vault.amount;
    require!(
        amount > 0 && amount <= vault_amount,
        ErrorCode::InvalidSweepAmount
    );

    let bump = ctx.accounts.sale_config.bump;
    let usdc_decimals = ctx.accounts.sale_config.usdc_decimals;
    let nonce_bytes = sale_nonce.to_le_bytes();
    let signer_seeds: &[&[u8]] = &[SALE_CONFIG_SEED, &nonce_bytes, &[bump]];

    transfer_checked(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: ctx.accounts.usdc_vault.to_account_info(),
                mint: ctx.accounts.usdc_mint.to_account_info(),
                to: ctx.accounts.treasury.to_account_info(),
                authority: ctx.accounts.sale_config.to_account_info(),
            },
            &[signer_seeds],
        ),
        amount,
        usdc_decimals,
    )?;

    emit!(ProceedsSwept {
        sale_config: ctx.accounts.sale_config.key(),
        treasury: ctx.accounts.treasury.key(),
        amount,
        vault_remaining: vault_amount - amount,
    });
    Ok(())
}
```

- [ ] **Step 3: Register the module and entry point**

In `instructions.rs` add `pub mod sweep_proceeds;` and `pub use sweep_proceeds::*;` (alphabetical placement, after `refund`'s former slot). In `lib.rs`, inside `pub mod presale`, add:

```rust
    pub fn sweep_proceeds(
        ctx: Context<SweepProceeds>,
        sale_nonce: u64,
        amount: u64,
    ) -> Result<()> {
        crate::instructions::sweep_proceeds::handle_sweep_proceeds(ctx, sale_nonce, amount)
    }
```

- [ ] **Step 4: Add sweep tests**

In `programs/tests/presale.ts`, add a describe block. It must cover: a valid admin sweep moves USDC and leaves claims payable; a non-admin sweep is rejected; a sweep to a non-treasury account is rejected; an over-balance sweep is rejected.

```typescript
describe("sweep_proceeds", () => {
  it("lets the admin sweep USDC to the fixed treasury while the sale is live, and claims still pay", async () => {
    const nonce = nextNonce();
    const treasuryOwner = Keypair.generate();
    const treasury = await ata(usdcMint, treasuryOwner.publicKey);
    await initSale(nonce, {
      hardCap: usdcUnit(1_000_000),
      softCap: new BN(0),
      minContribution: usdcUnit(1),
      maxContribution: usdcUnit(1_000_000),
      maxSlippageBps: 100,
      startOffset: -5,
      endOffset: 3600,
      whitelist: [],
      treasury, // initSale must accept an override treasury; see note below
    });

    const buyer = Keypair.generate();
    await airdrop(buyer.publicKey);
    const buyerUsdc = await ata(usdcMint, buyer.publicKey);
    const buyerOpen = await ata(openMint, buyer.publicKey);
    await mintTo9or6(usdcMint, buyerUsdc, usdcUnit(1_000));
    await contributeUsdc(nonce, buyer, buyerUsdc, usdcUnit(200));

    // Admin sweeps 150 USDC of the 200 raised.
    await program.methods
      .sweepProceeds(new BN(nonce), usdcUnit(150))
      .accounts({
        admin: admin.publicKey,
        saleConfig: saleConfigPda(nonce),
        usdcVault: usdcVaultPda(nonce),
        treasury,
        usdcMint,
        tokenProgram: TOKEN_2022_PROGRAM_ID,
      })
      .rpc({ commitment: "confirmed" });

    expect((await getAccount(connection, treasury, "confirmed", TOKEN_2022_PROGRAM_ID)).amount.toString())
      .to.equal(usdcUnit(150).toString());

    // The buyer can still claim their full 200 OPEN — claims survive sweeps.
    await claim(nonce, buyer, buyerOpen);
    expect((await getAccount(connection, buyerOpen, "confirmed", TOKEN_2022_PROGRAM_ID)).amount.toString())
      .to.equal(openUnit(200).toString());
  });

  it("rejects a sweep from a non-admin signer", async () => {
    const nonce = nextNonce();
    const treasuryOwner = Keypair.generate();
    const treasury = await ata(usdcMint, treasuryOwner.publicKey);
    await initSale(nonce, { hardCap: usdcUnit(1_000), softCap: new BN(0), minContribution: usdcUnit(1), maxContribution: usdcUnit(1_000), maxSlippageBps: 100, startOffset: -5, endOffset: 3600, whitelist: [], treasury });
    const attacker = Keypair.generate();
    await airdrop(attacker.publicKey);
    await expectAnchorError(
      program.methods.sweepProceeds(new BN(nonce), usdcUnit(1))
        .accounts({ admin: attacker.publicKey, saleConfig: saleConfigPda(nonce), usdcVault: usdcVaultPda(nonce), treasury, usdcMint, tokenProgram: TOKEN_2022_PROGRAM_ID })
        .signers([attacker]).rpc({ commitment: "confirmed" }),
      "Unauthorized",
    );
  });

  it("rejects a sweep whose destination is not sale_config.treasury", async () => {
    const nonce = nextNonce();
    const treasuryOwner = Keypair.generate();
    const treasury = await ata(usdcMint, treasuryOwner.publicKey);
    await initSale(nonce, { hardCap: usdcUnit(1_000), softCap: new BN(0), minContribution: usdcUnit(1), maxContribution: usdcUnit(1_000), maxSlippageBps: 100, startOffset: -5, endOffset: 3600, whitelist: [], treasury });
    const evilOwner = Keypair.generate();
    const evil = await ata(usdcMint, evilOwner.publicKey);
    // Anchor raises a constraint error (ConstraintRaw / a has_one-style
    // address mismatch) when treasury != sale_config.treasury.
    await expectAnchorError(
      program.methods.sweepProceeds(new BN(nonce), usdcUnit(1))
        .accounts({ admin: admin.publicKey, saleConfig: saleConfigPda(nonce), usdcVault: usdcVaultPda(nonce), treasury: evil, usdcMint, tokenProgram: TOKEN_2022_PROGRAM_ID })
        .rpc({ commitment: "confirmed" }),
      "ConstraintRaw",
    );
  });

  it("rejects a sweep larger than the vault balance", async () => {
    const nonce = nextNonce();
    const treasuryOwner = Keypair.generate();
    const treasury = await ata(usdcMint, treasuryOwner.publicKey);
    await initSale(nonce, { hardCap: usdcUnit(1_000), softCap: new BN(0), minContribution: usdcUnit(1), maxContribution: usdcUnit(1_000), maxSlippageBps: 100, startOffset: -5, endOffset: 3600, whitelist: [], treasury });
    await expectAnchorError(
      program.methods.sweepProceeds(new BN(nonce), usdcUnit(1))
        .accounts({ admin: admin.publicKey, saleConfig: saleConfigPda(nonce), usdcVault: usdcVaultPda(nonce), treasury, usdcMint, tokenProgram: TOKEN_2022_PROGRAM_ID })
        .rpc({ commitment: "confirmed" }),
      "InvalidSweepAmount",
    );
  });
});
```

Note: if the current `initSale` helper hard-codes a single shared treasury, extend it to accept an optional `treasury` override in its params object (defaulting to the existing shared treasury) so these tests can point at a per-test treasury. Confirm the exact constraint-error string Anchor emits for the wrong-treasury case by reading the first failing run's log and matching it (`ConstraintRaw` is the expected code for a `constraint = ...` violation in this Anchor version).

- [ ] **Step 5: Run the suite to verify green**

Run (from `programs/`): `anchor test --validator legacy`
Expected: PASS, including all four `sweep_proceeds` cases.

- [ ] **Step 6: Commit and push**

```bash
cd /OpenFiat/openfiat-core
git add programs/programs/presale/src programs/tests/presale.ts
git commit -m "feat(presale): add admin sweep_proceeds to fixed treasury with ProceedsSwept event"
git push origin main
```

---

### Task 4: Devnet re-proof + CONFORMANCE record

Redeploy to devnet and prove the new lifecycle end-to-end against real on-chain state.

**Files:**
- Create: `programs/scripts/prove-devnet-presale-claim-sweep.ts`
- Modify: `CONFORMANCE.md` (record the proof + signatures)

**Interfaces:**
- Consumes: the deployed presale program (id `75rJ9MRAaSnAc8tg4AfeTFVDCVrN6jdD5CqeyE4UoUw7`), the devnet OPEN mint `29w8TroBTYoaqrXBDcpv5L54VZRA8Kf7kU5U1cakvFdj`, and the Community Presale vault from `devnet-addresses.json`.

- [ ] **Step 1: Build and redeploy the presale program to devnet (in-place upgrade)**

```bash
cd /OpenFiat/openfiat-core/programs
anchor build -p presale
solana program deploy \
  --program-id target/deploy/presale-keypair.json \
  --url https://api.devnet.solana.com \
  target/deploy/presale.so
```

Expected: an upgrade signature; program data at `75rJ9MRAaSnAc8tg4AfeTFVDCVrN6jdD5CqeyE4UoUw7`. (The upgrade authority is `EA8TyQ58C3eavg3ThRFTMu1KLyV9e1v2oTQubSBQ9s5z` per `devnet-addresses.json`; use that keypair's Solana CLI config.)

- [ ] **Step 2: Write the devnet proof script**

Create `programs/scripts/prove-devnet-presale-claim-sweep.ts` modeled on the existing `programs/scripts/init-devnet-sale.ts` and `finalize-devnet-sale-for-testing.ts` (reuse their provider/connection/keypair-loading and `devnet-addresses.json` reads). The script must, under a **new `sale_nonce`** (e.g. current unix seconds, to avoid colliding with existing devnet `SaleConfig` PDAs) with `soft_cap = 0`:
1. `initialize_sale` (treasury = a devnet USDC ATA the script controls; `swap_program` = `JUPITER_V6_PROGRAM_ID`).
2. Fund a fresh buyer with devnet USDC; `contribute_usdc` a small amount.
3. `claim` **while Active**; assert the buyer's OPEN ATA received the entitlement.
4. `contribute_usdc` again; `claim` again; assert only the delta arrived.
5. `sweep_proceeds` part of the balance to the treasury; assert the treasury balance and log the `ProceedsSwept` event.
6. Print every transaction signature.

Guard the script so it refuses to run against any cluster whose RPC URL is not `api.devnet.solana.com` (mirror the safety check style in the existing devnet scripts) — it must never touch mainnet.

- [ ] **Step 3: Run the proof script against devnet**

```bash
cd /OpenFiat/openfiat-core/programs
npx ts-node scripts/prove-devnet-presale-claim-sweep.ts
```

Expected: all asserts pass; a list of signatures printed for init / contribute / claim(active) / contribute / claim(delta) / sweep.

- [ ] **Step 4: Record the proof in CONFORMANCE.md**

Add a subsection under the presale section noting: claim-anytime and `sweep_proceeds` proven on devnet on 2026-08-07, with the six signatures from Step 3 and the sale_nonce used. Keep the existing presale proof entries.

- [ ] **Step 5: Commit and push**

```bash
cd /OpenFiat/openfiat-core
git add programs/scripts/prove-devnet-presale-claim-sweep.ts CONFORMANCE.md
git commit -m "test(presale): prove claim-anytime + sweep_proceeds on devnet"
git push origin main
```

---

## Self-Review

**Spec coverage** (against `2026-08-07-presale-claim-anytime-sweep-design.md`):
- Claim ungated from finalize → Task 2. ✓
- `claimed_open` high-water mark + `NothingToClaim` → Task 2. ✓
- `sweep_proceeds` admin-gated, destination = fixed treasury, `ProceedsSwept` event, Active-only, amount-bounded → Task 3. ✓
- Remove `refund` + `SoftCapMissed`; `initialize_sale` (and `update_sale_params`) reject `soft_cap != 0`; finalize always `Finalized` → Task 1. ✓
- Invariants (no oversell, claims survive sweeps, no double-claim, no refund path) → asserted in Task 2 (delta/NothingToClaim) and Task 3 (claim-after-sweep). ✓
- `Contribution` layout change acceptable via fresh deploy / new devnet nonce → Task 2 + Task 4. ✓
- Tests enumerated in the spec → Tasks 1–3 test steps. ✓
- Devnet re-proof + CONFORMANCE → Task 4. ✓
- Ripple effects (doc/audit/deploy params) are tracked as sub-projects 1/3/4 — out of scope for this plan by design.

**Placeholder scan:** No TBD/TODO in code steps; the one deliberate runtime-confirmed value (the exact Anchor constraint-error string in Task 3 Step 4) is flagged with how to confirm it. ✓

**Type consistency:** `SaleState { Active, Finalized }` used consistently; `Contribution.claimed_open: u64` referenced in Task 2's claim handler; error variants appended in order `SoftCapNotSupported` (T1) → `NothingToClaim` (T2) → `InvalidSweepAmount` (T3), never inserted; `sweep_proceeds(sale_nonce, amount)` signature matches between `lib.rs` and the handler. ✓
