use anchor_lang::prelude::*;

use crate::{constants::*, error::ErrorCode, state::*};

/// Atomic counter marking only — no token movement (OFS-4200 §4).
///
/// Requires the merchant's own signature, matching every other
/// liquidity-vault-mutating instruction in this program: the off-chain
/// `reservations` crate (OFS-2200) already decides *whether* a given
/// buyer may reserve against a published ad's terms; this instruction
/// only executes that already-made decision on-chain, relayed by the
/// merchant's own node/wallet (the same "off-chain protocol decides,
/// on-chain program executes" split this workspace already uses for
/// chain-bridge transaction relay). A permissionless version would let
/// any caller lock up a merchant's advertised liquidity with no
/// corresponding real trade — a griefing vector this design avoids by
/// construction rather than by a runtime check.
#[derive(Accounts)]
pub struct ReserveLiquidity<'info> {
    pub merchant: Signer<'info>,

    #[account(
        mut,
        seeds = [LIQUIDITY_VAULT_SEED, merchant.key().as_ref(), liquidity_vault.mint.as_ref()],
        bump = liquidity_vault.bump,
        has_one = merchant,
    )]
    pub liquidity_vault: Account<'info, LiquidityVault>,
}

pub fn handle_reserve_liquidity(ctx: Context<ReserveLiquidity>, amount: u64) -> Result<()> {
    let liquidity_vault = &mut ctx.accounts.liquidity_vault;
    require!(
        liquidity_vault.available >= amount,
        ErrorCode::InsufficientAvailableLiquidity
    );
    liquidity_vault.available -= amount;
    liquidity_vault.reserved = liquidity_vault
        .reserved
        .checked_add(amount)
        .ok_or(ErrorCode::Overflow)?;
    Ok(())
}
