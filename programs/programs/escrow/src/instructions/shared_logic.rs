//! Fund-movement helpers shared by more than one instruction — kept out
//! of any single instruction file so `cancel_reservation` and
//! `expire_reservation` (identical unwind logic, different callers)
//! don't duplicate the CPI/accounting.

use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    transfer_checked, Mint, Token2022, TokenAccount, TransferChecked,
};

use crate::{constants::TRADE_ESCROW_SEED, error::ErrorCode, state::*};

/// Releases a reservation-marking without ever having funded a trade
/// escrow (e.g. cancelled/expired before `fund_trade_escrow` ran).
pub fn release_reservation_marking(
    liquidity_vault: &mut Account<LiquidityVault>,
    amount: u64,
) -> Result<()> {
    require!(
        liquidity_vault.reserved >= amount,
        ErrorCode::InsufficientReservedLiquidity
    );
    liquidity_vault.reserved -= amount;
    liquidity_vault.available = liquidity_vault
        .available
        .checked_add(amount)
        .ok_or(ErrorCode::Overflow)?;
    Ok(())
}

/// Reverses a `fund_trade_escrow` — returns an already-funded trade
/// escrow's tokens to the liquidity vault's `available` balance. Used by
/// both `cancel_reservation` and `expire_reservation` for the case where
/// the escrow had already been funded before it was unwound.
#[allow(clippy::too_many_arguments)]
pub fn unwind_funded_trade_escrow<'info>(
    trade_escrow: &Account<'info, TradeEscrowVault>,
    trade_escrow_token_vault: &InterfaceAccount<'info, TokenAccount>,
    liquidity_vault: &mut Account<'info, LiquidityVault>,
    liquidity_token_vault: &InterfaceAccount<'info, TokenAccount>,
    mint: &InterfaceAccount<'info, Mint>,
    token_program: &Program<'info, Token2022>,
) -> Result<()> {
    let amount = trade_escrow.amount;
    let id_bytes = trade_escrow.reservation_id.to_le_bytes();
    let bump = trade_escrow.bump;
    let signer_seeds: &[&[u8]] = &[TRADE_ESCROW_SEED, &id_bytes, &[bump]];

    transfer_checked(
        CpiContext::new_with_signer(
            token_program.key(),
            TransferChecked {
                from: trade_escrow_token_vault.to_account_info(),
                mint: mint.to_account_info(),
                to: liquidity_token_vault.to_account_info(),
                authority: trade_escrow.to_account_info(),
            },
            &[signer_seeds],
        ),
        amount,
        mint.decimals,
    )?;

    liquidity_vault.pending_settlement -= amount;
    liquidity_vault.available = liquidity_vault
        .available
        .checked_add(amount)
        .ok_or(ErrorCode::Overflow)?;
    Ok(())
}
