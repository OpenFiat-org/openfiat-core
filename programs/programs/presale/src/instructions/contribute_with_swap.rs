use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::solana_program::program::invoke;
use anchor_spl::token_interface::{Mint, TokenAccount};

use crate::{constants::*, error::ErrorCode, state::*};

/// Contribute using SOL or a whitelisted, non-USDC stablecoin, converted to
/// USDC atomically via CPI into the sale's configured swap-aggregator
/// program (OFS-4100 §3, OFS-4200 §3 — the plan's flagged highest-risk
/// component). Use `contribute_usdc` instead for direct USDC contributions.
///
/// # Why this is safe without hardcoding Jupiter's account layout
///
/// `sale_config.swap_program` is checked by pubkey equality — no other
/// program can be substituted. Beyond that, this instruction does not trust
/// anything about *how* the swap moves funds internally: `remaining_accounts`
/// (the swap route — pool/AMM accounts, varies per quote) is supplied
/// verbatim by the client, which assembled it from the swap aggregator's own
/// official quote/swap API. The only thing this instruction verifies is the
/// *result*: `usdc_vault`'s balance, reloaded after the CPI returns, must
/// have increased by at least the caller's own computed slippage floor. If a
/// caller supplies a wrong/malicious route, the CPI either fails outright
/// (whole transaction reverts, Solana's execution model is atomic — no
/// partial contribution can ever be recorded) or succeeds without crediting
/// `usdc_vault` enough, which fails the same check. There is no path by
/// which a caller can cause OPEN to be credited without `usdc_vault`
/// actually receiving the required USDC.
///
/// `source_mint`'s whitelist check is a policy/UX constraint (which asset
/// types this presale advertises as accepted), not a security boundary: a
/// caller who lies about `source_mint` can only ever misspend their own
/// funds, never extract value from the sale.
#[derive(Accounts)]
#[instruction(sale_nonce: u64)]
pub struct ContributeWithSwap<'info> {
    #[account(mut)]
    pub buyer: Signer<'info>,

    #[account(
        mut,
        seeds = [SALE_CONFIG_SEED, &sale_nonce.to_le_bytes()],
        bump = sale_config.bump,
        constraint = sale_config.usdc_vault == usdc_vault.key(),
    )]
    pub sale_config: Account<'info, SaleConfig>,

    /// The asset being contributed — must be wSOL or on the stablecoin
    /// whitelist, and must not be USDC (use `contribute_usdc` for that).
    pub source_mint: InterfaceAccount<'info, Mint>,

    #[account(mut)]
    pub usdc_vault: InterfaceAccount<'info, TokenAccount>,

    #[account(
        init_if_needed,
        payer = buyer,
        space = 8 + Contribution::INIT_SPACE,
        seeds = [CONTRIBUTION_SEED, sale_config.key().as_ref(), buyer.key().as_ref()],
        bump
    )]
    pub contribution: Account<'info, Contribution>,

    /// CHECK: verified by pubkey equality against `sale_config.swap_program`
    /// below. The actual swap accounts are `remaining_accounts`.
    pub swap_program: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
    // token_program is implicitly among remaining_accounts / not otherwise
    // needed here since this instruction performs no direct token::transfer
    // of its own — the swap CPI performs all token movement.
}

pub fn handle_contribute_with_swap(
    ctx: Context<ContributeWithSwap>,
    _sale_nonce: u64,
    expected_usdc_out: u64,
    swap_instruction_data: Vec<u8>,
) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let sale_config = &ctx.accounts.sale_config;

    require!(
        sale_config.state == SaleState::Active,
        ErrorCode::SaleNotActive
    );
    require!(now >= sale_config.start_time, ErrorCode::SaleNotStarted);
    require!(now <= sale_config.end_time, ErrorCode::SaleEnded);

    require!(
        ctx.accounts.source_mint.key() != sale_config.usdc_mint,
        ErrorCode::UseDirectUsdcPath
    );
    require!(
        ctx.accounts.source_mint.key() == WSOL_MINT
            || sale_config
                .stablecoin_whitelist
                .contains(&ctx.accounts.source_mint.key()),
        ErrorCode::AssetNotWhitelisted
    );
    require!(
        ctx.accounts.swap_program.key() == sale_config.swap_program,
        ErrorCode::SwapProgramMismatch
    );

    // Slippage floor: the caller's own quote, tightened by the sale's
    // configured tolerance. A caller cannot raise this floor to attack the
    // sale (only make their own swap more likely to revert), and cannot
    // lower it to steal from the sale (the balance-delta check below is the
    // sole source of truth, independent of this floor's derivation).
    let min_usdc_out = (expected_usdc_out as u128)
        .checked_mul((BPS_DENOMINATOR - sale_config.max_slippage_bps as u64) as u128)
        .ok_or(ErrorCode::Overflow)?
        .checked_div(BPS_DENOMINATOR as u128)
        .ok_or(ErrorCode::Overflow)? as u64;

    let usdc_before = ctx.accounts.usdc_vault.amount;

    let metas: Vec<AccountMeta> = ctx
        .remaining_accounts
        .iter()
        .map(|a| AccountMeta {
            pubkey: *a.key,
            is_signer: a.is_signer,
            is_writable: a.is_writable,
        })
        .collect();
    let ix = Instruction {
        program_id: ctx.accounts.swap_program.key(),
        accounts: metas,
        data: swap_instruction_data,
    };
    invoke(&ix, ctx.remaining_accounts)?;

    ctx.accounts.usdc_vault.reload()?;
    let usdc_after = ctx.accounts.usdc_vault.amount;
    let delta = usdc_after
        .checked_sub(usdc_before)
        .ok_or(ErrorCode::Overflow)?;
    require!(delta >= min_usdc_out, ErrorCode::SlippageExceeded);

    let contribution = &ctx.accounts.contribution;
    let is_first_contribution = contribution.amount_usdc == 0;
    if is_first_contribution {
        require!(
            delta >= sale_config.min_contribution,
            ErrorCode::BelowMinimumContribution
        );
    }
    let new_wallet_total = contribution
        .amount_usdc
        .checked_add(delta)
        .ok_or(ErrorCode::Overflow)?;
    require!(
        new_wallet_total <= sale_config.max_contribution,
        ErrorCode::AboveMaximumContribution
    );
    let new_total_raised = sale_config
        .total_raised
        .checked_add(delta)
        .ok_or(ErrorCode::Overflow)?;
    require!(
        new_total_raised <= sale_config.hard_cap,
        ErrorCode::HardCapExceeded
    );

    let open_delta = sale_config.open_entitlement_for(delta)?;

    let contribution = &mut ctx.accounts.contribution;
    contribution.buyer = ctx.accounts.buyer.key();
    contribution.amount_usdc = new_wallet_total;
    contribution.open_entitlement = contribution
        .open_entitlement
        .checked_add(open_delta)
        .ok_or(ErrorCode::Overflow)?;
    contribution.bump = ctx.bumps.contribution;

    let sale_config = &mut ctx.accounts.sale_config;
    sale_config.total_raised = new_total_raised;

    Ok(())
}
