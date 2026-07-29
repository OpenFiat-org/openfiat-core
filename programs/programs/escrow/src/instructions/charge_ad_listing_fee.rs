use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    transfer_checked, Mint, Token2022, TokenAccount, TransferChecked,
};

use crate::{constants::*, error::ErrorCode, events::AdListingFeeCharged, state::*};

/// Charges a merchant the advertisement-listing fee against their OPEN
/// liquidity vault.
///
/// # Why this is a dedicated instruction
///
/// `FeeConfig.ad_listing_fee` existed from the start and was read by
/// nothing — stored at initialisation and never charged. The obvious
/// objection to fixing that is that advertisements are off-chain gossip
/// records (OFS-2100): the protocol deliberately keeps listings off-chain
/// per the whitepaper's "decentralize only what benefits from it", so
/// there is no on-chain advertisement object to hang a fee on.
///
/// What makes it chargeable anyway is that the *merchant* is on-chain.
/// A merchant already maintains a `LiquidityVault`, so the funds have an
/// identified source even though the listing itself does not exist here.
///
/// The alternatives were worse. Charging inside `create_liquidity_vault`
/// or `deposit_liquidity` bills for funding rather than for listing, and
/// a merchant funds once but lists many times. Charging inside
/// `reserve_liquidity` bills per *trade*, which is the settlement fee's
/// job and would double-charge the same activity. A dedicated call is the
/// only option that bills the thing actually being paid for, once per
/// listing.
///
/// `advertisement_id` is the off-chain record's own identifier, recorded
/// in the emitted event and nowhere else. This program stores no
/// advertisement state and draws no conclusion from the id — it is the
/// join key that lets an indexer match a payment to the gossiped listing
/// it paid for. Passing a duplicate id charges again; enforcing
/// one-payment-per-listing would mean holding a per-advertisement account
/// on-chain, which is the design this deliberately avoids.
///
/// # Denomination
///
/// The fee is denominated in OPEN (OFS-4100 §6), and the vault it is
/// drawn from must therefore be the merchant's OPEN vault, not the
/// stablecoin vault backing their trades. `LiquidityVault` is already
/// keyed `(merchant, mint)`, so this needs no new account type — a
/// merchant simply keeps an OPEN vault alongside their settlement one.
/// The `fee_mint` constraint below pins that.
#[derive(Accounts)]
pub struct ChargeAdListingFee<'info> {
    pub merchant: Signer<'info>,

    #[account(
        seeds = [FEE_CONFIG_SEED],
        bump = fee_config.bump,
        constraint = fee_config.dev_treasury == dev_treasury.key() @ ErrorCode::Unauthorized,
    )]
    pub fee_config: Account<'info, FeeConfig>,

    /// The merchant's OPEN vault. `mint` is the fee's denomination, which
    /// is what makes this the OPEN vault rather than a settlement one.
    #[account(
        mut,
        seeds = [LIQUIDITY_VAULT_SEED, merchant.key().as_ref(), mint.key().as_ref()],
        bump = liquidity_vault.bump,
        has_one = merchant,
        constraint = liquidity_vault.mint == mint.key(),
    )]
    pub liquidity_vault: Account<'info, LiquidityVault>,

    #[account(
        mut,
        seeds = [LIQUIDITY_VAULT_TOKENS_SEED, merchant.key().as_ref(), mint.key().as_ref()],
        bump = liquidity_vault.token_vault_bump,
    )]
    pub token_vault: InterfaceAccount<'info, TokenAccount>,

    /// Listing fees are protocol revenue, so they follow the same route as
    /// any other: the development treasury. Splitting them four ways like
    /// the settlement fee would need four more accounts on every listing
    /// for what is a single small flat charge.
    ///
    /// `[PROPOSED — NEEDS SIGN-OFF]` — OFS-4100 §6 sets the fee's amount
    /// but names no destination for it.
    #[account(mut, constraint = dev_treasury.mint == mint.key())]
    pub dev_treasury: InterfaceAccount<'info, TokenAccount>,

    pub mint: InterfaceAccount<'info, Mint>,

    pub token_program: Program<'info, Token2022>,
}

pub fn handle_charge_ad_listing_fee(
    ctx: Context<ChargeAdListingFee>,
    advertisement_id: [u8; 32],
) -> Result<()> {
    let amount = ctx.accounts.fee_config.ad_listing_fee;
    require!(amount > 0, ErrorCode::NoFeeConfigured);
    require!(
        ctx.accounts.liquidity_vault.available >= amount,
        ErrorCode::InsufficientAvailableLiquidity
    );

    let merchant_key = ctx.accounts.merchant.key();
    let mint_key = ctx.accounts.mint.key();
    let bump = ctx.accounts.liquidity_vault.bump;
    let signer_seeds: &[&[u8]] = &[
        LIQUIDITY_VAULT_SEED,
        merchant_key.as_ref(),
        mint_key.as_ref(),
        &[bump],
    ];

    transfer_checked(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: ctx.accounts.token_vault.to_account_info(),
                mint: ctx.accounts.mint.to_account_info(),
                to: ctx.accounts.dev_treasury.to_account_info(),
                authority: ctx.accounts.liquidity_vault.to_account_info(),
            },
            &[signer_seeds],
        ),
        amount,
        ctx.accounts.mint.decimals,
    )?;

    let liquidity_vault = &mut ctx.accounts.liquidity_vault;
    liquidity_vault.available = liquidity_vault
        .available
        .checked_sub(amount)
        .ok_or(ErrorCode::Overflow)?;
    liquidity_vault.total = liquidity_vault
        .total
        .checked_sub(amount)
        .ok_or(ErrorCode::Overflow)?;

    emit!(AdListingFeeCharged {
        merchant: merchant_key,
        advertisement_id,
        mint: mint_key,
        amount,
        vault_available: liquidity_vault.available,
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}
