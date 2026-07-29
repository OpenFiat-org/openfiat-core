//! Fund-movement helpers shared by more than one instruction — kept out
//! of any single instruction file so `cancel_reservation` and
//! `expire_reservation` (identical unwind logic, different callers)
//! don't duplicate the CPI/accounting.

use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    transfer_checked, Mint, Token2022, TokenAccount, TransferChecked,
};
use openfiat_programs_shared::VaultState;

use crate::constants::{BPS_DENOMINATOR, TRADE_ESCROW_SEED};
use crate::{error::ErrorCode, state::*};

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

/// Splits a funded trade escrow evenly between the buyer and the seller's
/// liquidity vault, taking no settlement fee.
///
/// Used for two outcomes that must not hand either party the whole
/// amount: a `MutualSettlement` verdict, and the terminal case where
/// arbitration has failed to reach a decision after
/// [`MAX_DISPUTE_ROUNDS`](crate::constants::MAX_DISPUTE_ROUNDS).
///
/// Even, rather than by some agreed ratio, because no ratio is recorded
/// anywhere on-chain — OFS-2400 §17's "Partial Settlement" is explicitly
/// future work. The even split is `[PROPOSED — NEEDS SIGN-OFF]`; what is
/// *not* negotiable is that neither of these cases may pay one side in
/// full, since that is precisely what made forcing indecision profitable.
///
/// No fee is charged: the protocol did not successfully arbitrate, so it
/// has not earned one. Also `[PROPOSED — NEEDS SIGN-OFF]`.
///
/// An odd unit goes to the seller's vault, chosen only so the division is
/// deterministic rather than for any economic reason.
#[allow(clippy::too_many_arguments)]
pub fn split_trade_escrow_evenly<'info>(
    trade_escrow: &mut Account<'info, TradeEscrowVault>,
    trade_escrow_token_vault: &InterfaceAccount<'info, TokenAccount>,
    buyer_token_account: &InterfaceAccount<'info, TokenAccount>,
    liquidity_vault: &mut Account<'info, LiquidityVault>,
    liquidity_token_vault: &InterfaceAccount<'info, TokenAccount>,
    mint: &InterfaceAccount<'info, Mint>,
    token_program: &Program<'info, Token2022>,
) -> Result<()> {
    let amount = trade_escrow.amount;
    let buyer_share = amount / 2;
    let seller_share = amount.checked_sub(buyer_share).ok_or(ErrorCode::Overflow)?;

    let id_bytes = trade_escrow.reservation_id.to_le_bytes();
    let bump = trade_escrow.bump;
    let signer_seeds: &[&[u8]] = &[TRADE_ESCROW_SEED, &id_bytes, &[bump]];

    let decimals = mint.decimals;
    let token_program_id = token_program.key();
    let mint_info = mint.to_account_info();
    let from = trade_escrow_token_vault.to_account_info();
    let authority = trade_escrow.to_account_info();

    let destinations: [(AccountInfo<'info>, u64); 2] = [
        (buyer_token_account.to_account_info(), buyer_share),
        (liquidity_token_vault.to_account_info(), seller_share),
    ];

    for (to, share_amount) in destinations {
        if share_amount == 0 {
            continue;
        }
        transfer_checked(
            CpiContext::new_with_signer(
                token_program_id,
                TransferChecked {
                    from: from.clone(),
                    mint: mint_info.clone(),
                    to,
                    authority: authority.clone(),
                },
                &[signer_seeds],
            ),
            share_amount,
            decimals,
        )?;
    }

    trade_escrow.state = VaultState::Released;
    liquidity_vault.pending_settlement -= amount;
    liquidity_vault.available = liquidity_vault
        .available
        .checked_add(seller_share)
        .ok_or(ErrorCode::Overflow)?;
    liquidity_vault.settled = liquidity_vault
        .settled
        .checked_add(buyer_share)
        .ok_or(ErrorCode::Overflow)?;
    Ok(())
}

/// Splits `amount` into (buyer_amount, [dev, ecosystem, infra, emergency]),
/// per `FeeConfig`'s settlement fee rate and 4-way basis-point split.
/// Shared by `release_escrow` and `execute_dispute_outcome`'s `BuyerWins`
/// path — both release funds identically once a release is authorized.
pub fn compute_fee_split(fee_config: &FeeConfig, amount: u64) -> Result<(u64, [u64; 4])> {
    let fee = (amount as u128)
        .checked_mul(fee_config.settlement_fee_bps as u128)
        .ok_or(ErrorCode::Overflow)?
        .checked_div(BPS_DENOMINATOR as u128)
        .ok_or(ErrorCode::Overflow)? as u64;
    let buyer_amount = amount.checked_sub(fee).ok_or(ErrorCode::Overflow)?;

    let splits = [
        fee_config.dev_treasury_bps,
        fee_config.ecosystem_treasury_bps,
        fee_config.infra_treasury_bps,
        fee_config.emergency_reserve_bps,
    ];
    let mut shares = [0u64; 4];
    let mut allocated = 0u64;
    for (i, bps) in splits.iter().enumerate() {
        let share = (fee as u128)
            .checked_mul(*bps as u128)
            .ok_or(ErrorCode::Overflow)?
            .checked_div(BPS_DENOMINATOR as u128)
            .ok_or(ErrorCode::Overflow)? as u64;
        shares[i] = share;
        allocated = allocated.checked_add(share).ok_or(ErrorCode::Overflow)?;
    }
    // Rounding remainder (basis-point division truncates) goes to the
    // emergency reserve rather than being silently lost.
    let remainder = fee.checked_sub(allocated).ok_or(ErrorCode::Overflow)?;
    shares[3] = shares[3]
        .checked_add(remainder)
        .ok_or(ErrorCode::Overflow)?;

    Ok((buyer_amount, shares))
}

/// Moves an approved (or dispute-resolved `BuyerWins`) trade escrow's
/// funds to the buyer plus the fee split's treasury destinations, and
/// updates `liquidity_vault`'s settled/pending counters. Shared by
/// `release_escrow` and `execute_dispute_outcome`.
#[allow(clippy::too_many_arguments)]
pub fn release_trade_escrow_funds<'info>(
    trade_escrow: &mut Account<'info, TradeEscrowVault>,
    trade_escrow_token_vault: &InterfaceAccount<'info, TokenAccount>,
    buyer_token_account: &InterfaceAccount<'info, TokenAccount>,
    liquidity_vault: &mut Account<'info, LiquidityVault>,
    fee_config: &FeeConfig,
    dev_treasury: &InterfaceAccount<'info, TokenAccount>,
    ecosystem_treasury: &InterfaceAccount<'info, TokenAccount>,
    infra_treasury: &InterfaceAccount<'info, TokenAccount>,
    emergency_reserve: &InterfaceAccount<'info, TokenAccount>,
    mint: &InterfaceAccount<'info, Mint>,
    token_program: &Program<'info, Token2022>,
) -> Result<(u64, [u64; 4])> {
    let amount = trade_escrow.amount;
    let (buyer_amount, fee_shares) = compute_fee_split(fee_config, amount)?;

    let id_bytes = trade_escrow.reservation_id.to_le_bytes();
    let bump = trade_escrow.bump;
    let signer_seeds: &[&[u8]] = &[TRADE_ESCROW_SEED, &id_bytes, &[bump]];

    let decimals = mint.decimals;
    let token_program_id = token_program.key();
    let mint_info = mint.to_account_info();
    let from = trade_escrow_token_vault.to_account_info();
    let authority = trade_escrow.to_account_info();

    let destinations: [(AccountInfo<'info>, u64); 5] = [
        (buyer_token_account.to_account_info(), buyer_amount),
        (dev_treasury.to_account_info(), fee_shares[0]),
        (ecosystem_treasury.to_account_info(), fee_shares[1]),
        (infra_treasury.to_account_info(), fee_shares[2]),
        (emergency_reserve.to_account_info(), fee_shares[3]),
    ];

    for (to, share_amount) in destinations {
        if share_amount == 0 {
            continue;
        }
        transfer_checked(
            CpiContext::new_with_signer(
                token_program_id,
                TransferChecked {
                    from: from.clone(),
                    mint: mint_info.clone(),
                    to,
                    authority: authority.clone(),
                },
                &[signer_seeds],
            ),
            share_amount,
            decimals,
        )?;
    }

    trade_escrow.state = VaultState::Released;
    liquidity_vault.pending_settlement -= amount;
    liquidity_vault.settled = liquidity_vault
        .settled
        .checked_add(amount)
        .ok_or(ErrorCode::Overflow)?;
    Ok((buyer_amount, fee_shares))
}
