use anchor_lang::prelude::*;
// `Ids` is what exposes `TokenInterface::ids()` — the same two program ids
// Anchor's `Interface` validates against — so the manual owner check below
// cannot drift from the one the account types apply.
use anchor_lang::Ids;
use anchor_spl::token_interface::{Mint, TokenAccount, TokenInterface};

use crate::{constants::*, error::ErrorCode, state::*};

/// Creates a merchant's pooled vault for one mint.
///
/// # This instruction creates two different kinds of vault
///
/// It reads as a settlement-liquidity instruction and it is mostly used as
/// one, but the same instruction — same seeds, same account, same handler —
/// is also how a merchant's **OPEN** vault comes into existence. That vault
/// is not settlement liquidity at all: it is what `charge_ad_listing_fee`
/// debits a listing fee from and what `open_dispute_case` takes an
/// arbitration deposit from. Nothing in the type system distinguishes the
/// two; the mint is the only difference.
///
/// `[CONFIRMED — protocol steward]` for the carve-out below, not for the
/// allowlist itself, which the directive already fixed.
///
/// The directive fixes which mints may be *settled* in and says nothing about
/// vault creation, because it was written without the knowledge that one
/// instruction builds both kinds of vault. Applying the allowlist here
/// unconditionally is the reading that follows the directive's letter, and it
/// is the reading this code deliberately does NOT take. The deviation was put
/// to the steward with its consequences — ad listing uncallable, arbitration
/// deposits silently zero — and ratified.
///
/// Recorded as a decision rather than deleted as resolved: the next reader to
/// notice an arbitration account inside a vault-creation instruction will
/// assume it is a mistake, and the reasoning is the only thing that stops
/// them "fixing" it.
///
/// That is why the settlement allowlist is not applied here unconditionally.
/// The protocol steward's directive puts wSOL, USDC and USDT on the list and
/// deliberately leaves OPEN off it until public sale. Refusing every
/// off-list mint here would therefore make an OPEN vault impossible to
/// create, which would make `charge_ad_listing_fee` uncallable and would
/// silently reduce every arbitration deposit to zero — `open_dispute_case`
/// takes what the vault can cover and opens the case anyway rather than
/// letting an underfunded merchant block a buyer's dispute, so the failure
/// would not surface as an error. It would surface as arbitrators no longer
/// being paid, which is the disincentive half of arbitration.
///
/// See [`openfiat_programs_shared::token_dispatch`] for the related reason
/// the token program is now an `Interface` rather than a fixed program.
#[derive(Accounts)]
pub struct CreateLiquidityVault<'info> {
    #[account(mut)]
    pub merchant: Signer<'info>,

    #[account(mint::token_program = token_program)]
    pub mint: InterfaceAccount<'info, Mint>,

    /// Carries the settlement allowlist `mint` is checked against.
    #[account(seeds = [FEE_CONFIG_SEED], bump = fee_config.bump)]
    pub fee_config: Box<Account<'info, FeeConfig>>,

    /// Present so this instruction can recognise the OPEN mint, and for no
    /// other reason — it is never read, written or transferred here.
    ///
    /// # Why an arbitration account appears in a vault-creation instruction
    ///
    /// The carve-out above needs to answer "is this mint OPEN?", and the
    /// escrow program has no `open_mint` field anywhere to answer it from.
    /// It does have one existing, unambiguous definition of OPEN: the
    /// arbitration pool is a token account, a token account holds exactly
    /// one mint, and the pool's mint *is* the OPEN the whole arbitration
    /// path is denominated in (`initialize_arbitration_pool` pins it, and
    /// `open_dispute_case` and `execute_dispute_outcome` both constrain
    /// against it). Reading it here reuses that definition rather than
    /// introducing a second one that could drift from it.
    ///
    /// It cannot be spoofed: `seeds = [ARBITRATION_POOL_SEED]` is a fixed
    /// singleton address, so Anchor re-derives it and there is exactly one
    /// account that satisfies the constraint. A caller cannot supply some
    /// other token account holding a mint of their choosing and thereby
    /// nominate their own "OPEN".
    ///
    /// **If this account is removed, the carve-out goes with it** and the
    /// allowlist becomes unconditional — which breaks the OPEN vault in the
    /// way this instruction's own doc describes. Replacing it with an
    /// `open_mint` field on `FeeConfig` would work and would cost a second
    /// singleton migration plus a second source of truth for OPEN; that
    /// trade was considered and declined.
    ///
    /// It is `UncheckedAccount` rather than a typed one so that it may be
    /// **absent**. Only the carve-out reads it, so on a chain where
    /// `initialize_arbitration_pool` has not run yet every allowlisted
    /// settlement mint still works exactly as before; the new ordering
    /// dependency applies to the OPEN path alone, which is the one that
    /// needs the pool anyway.
    ///
    /// CHECK: deserialized by hand below, so a missing pool reports
    /// `ArbitrationPoolNotInitialized` — naming the instruction the operator
    /// has to run — instead of a bare `AccountNotInitialized` on an account
    /// the caller never asked about and did not know was involved.
    #[account(seeds = [ARBITRATION_POOL_SEED], bump)]
    pub arbitration_pool: UncheckedAccount<'info>,

    #[account(
        init,
        payer = merchant,
        space = 8 + LiquidityVault::INIT_SPACE,
        seeds = [LIQUIDITY_VAULT_SEED, merchant.key().as_ref(), mint.key().as_ref()],
        bump
    )]
    pub liquidity_vault: Account<'info, LiquidityVault>,

    #[account(
        init,
        payer = merchant,
        seeds = [LIQUIDITY_VAULT_TOKENS_SEED, merchant.key().as_ref(), mint.key().as_ref()],
        bump,
        token::mint = mint,
        token::authority = liquidity_vault,
        token::token_program = token_program,
    )]
    pub token_vault: InterfaceAccount<'info, TokenAccount>,

    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

pub fn handle_create_liquidity_vault(ctx: Context<CreateLiquidityVault>) -> Result<()> {
    let mint = ctx.accounts.mint.key();

    if !ctx.accounts.fee_config.allows_settlement_mint(&mint) {
        // Not a settlement mint. The only other vault this program has a
        // use for is the merchant's OPEN one, so the mint has to be OPEN.
        //
        // Owner-checked and then properly deserialized, rather than read at
        // a hardcoded offset. The address is already pinned by seeds, so
        // both checks are belt and braces — but the entire carve-out rests
        // on this one mint comparison, and a hand-rolled offset read of an
        // account whose type was never verified is exactly how a check like
        // that stops meaning what it says.
        let pool_info = ctx.accounts.arbitration_pool.to_account_info();
        require!(
            TokenInterface::ids().contains(pool_info.owner),
            ErrorCode::ArbitrationPoolNotInitialized
        );
        let pool_mint = {
            let data = pool_info.try_borrow_data()?;
            TokenAccount::try_deserialize(&mut &data[..])
                .map_err(|_| error!(ErrorCode::ArbitrationPoolNotInitialized))?
                .mint
        };
        require_keys_eq!(mint, pool_mint, ErrorCode::SettlementMintNotAllowed);
    }

    let liquidity_vault = &mut ctx.accounts.liquidity_vault;
    liquidity_vault.merchant = ctx.accounts.merchant.key();
    liquidity_vault.mint = mint;
    liquidity_vault.total = 0;
    liquidity_vault.reserved = 0;
    liquidity_vault.available = 0;
    liquidity_vault.settled = 0;
    liquidity_vault.pending_settlement = 0;
    liquidity_vault.bump = ctx.bumps.liquidity_vault;
    liquidity_vault.token_vault_bump = ctx.bumps.token_vault;
    Ok(())
}
