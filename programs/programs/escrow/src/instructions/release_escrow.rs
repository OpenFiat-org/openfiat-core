use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    transfer_checked, Mint, Token2022, TokenAccount, TransferChecked,
};
use openfiat_programs_shared::VaultState;

use crate::{constants::*, error::ErrorCode, state::*};

/// The only instruction that moves settlement funds (OFS-2300 §16).
/// Permissionless once `approve_settlement` has run — matching "the
/// merchant never manually transfers stablecoins; only the OpenFiat
/// Program releases escrow" (OFS-2300 §15), there is no further
/// merchant action required after approval, so anyone (the buyer, a
/// relaying node, an automated cranker) may trigger the release.
#[derive(Accounts)]
pub struct ReleaseEscrow<'info> {
    pub mint: InterfaceAccount<'info, Mint>,

    #[account(
        mut,
        seeds = [LIQUIDITY_VAULT_SEED, trade_escrow.seller.as_ref(), mint.key().as_ref()],
        bump = liquidity_vault.bump,
        constraint = liquidity_vault.mint == mint.key(),
    )]
    pub liquidity_vault: Box<Account<'info, LiquidityVault>>,

    #[account(
        mut,
        seeds = [TRADE_ESCROW_SEED, &trade_escrow.reservation_id.to_le_bytes()],
        bump = trade_escrow.bump,
        constraint = trade_escrow.mint == mint.key(),
        constraint = trade_escrow.state == VaultState::AwaitingFiatSettlement @ ErrorCode::InvalidVaultState,
        constraint = trade_escrow.approved @ ErrorCode::InvalidVaultState,
    )]
    pub trade_escrow: Box<Account<'info, TradeEscrowVault>>,

    #[account(
        mut,
        seeds = [TRADE_ESCROW_TOKENS_SEED, &trade_escrow.reservation_id.to_le_bytes()],
        bump = trade_escrow.token_vault_bump,
    )]
    pub trade_escrow_token_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut, constraint = buyer_token_account.owner == trade_escrow.buyer, constraint = buyer_token_account.mint == mint.key())]
    pub buyer_token_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        seeds = [FEE_CONFIG_SEED],
        bump = fee_config.bump,
        constraint = fee_config.dev_treasury == dev_treasury.key(),
        constraint = fee_config.ecosystem_treasury == ecosystem_treasury.key(),
        constraint = fee_config.infra_treasury == infra_treasury.key(),
        constraint = fee_config.emergency_reserve == emergency_reserve.key(),
    )]
    pub fee_config: Box<Account<'info, FeeConfig>>,

    #[account(mut)]
    pub dev_treasury: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub ecosystem_treasury: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub infra_treasury: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub emergency_reserve: Box<InterfaceAccount<'info, TokenAccount>>,

    pub token_program: Program<'info, Token2022>,
}

/// Splits `amount` into (buyer_amount, [dev, ecosystem, infra, emergency]),
/// per `FeeConfig`'s settlement fee rate and 4-way basis-point split.
fn compute_fee_split(fee_config: &FeeConfig, amount: u64) -> Result<(u64, [u64; 4])> {
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

pub fn handle_release_escrow(ctx: Context<ReleaseEscrow>) -> Result<()> {
    let amount = ctx.accounts.trade_escrow.amount;
    let (buyer_amount, fee_shares) = compute_fee_split(&ctx.accounts.fee_config, amount)?;

    let reservation_id = ctx.accounts.trade_escrow.reservation_id;
    let id_bytes = reservation_id.to_le_bytes();
    let bump = ctx.accounts.trade_escrow.bump;
    let signer_seeds: &[&[u8]] = &[TRADE_ESCROW_SEED, &id_bytes, &[bump]];

    let decimals = ctx.accounts.mint.decimals;
    let token_program_id = ctx.accounts.token_program.key();
    let mint_info = ctx.accounts.mint.to_account_info();
    let from = ctx.accounts.trade_escrow_token_vault.to_account_info();
    let authority = ctx.accounts.trade_escrow.to_account_info();

    let destinations: [(anchor_lang::prelude::AccountInfo, u64); 5] = [
        (
            ctx.accounts.buyer_token_account.to_account_info(),
            buyer_amount,
        ),
        (ctx.accounts.dev_treasury.to_account_info(), fee_shares[0]),
        (
            ctx.accounts.ecosystem_treasury.to_account_info(),
            fee_shares[1],
        ),
        (ctx.accounts.infra_treasury.to_account_info(), fee_shares[2]),
        (
            ctx.accounts.emergency_reserve.to_account_info(),
            fee_shares[3],
        ),
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

    ctx.accounts.trade_escrow.state = VaultState::Released;

    let liquidity_vault = &mut ctx.accounts.liquidity_vault;
    liquidity_vault.pending_settlement -= amount;
    liquidity_vault.settled = liquidity_vault
        .settled
        .checked_add(amount)
        .ok_or(ErrorCode::Overflow)?;
    Ok(())
}
