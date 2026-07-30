use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    transfer_checked, Mint, TokenAccount, TokenInterface, TransferChecked,
};

use crate::{constants::*, error::ErrorCode, state::*};

#[derive(Accounts)]
#[instruction(sale_nonce: u64)]
pub struct ContributeUsdc<'info> {
    #[account(mut)]
    pub buyer: Signer<'info>,

    /// CHECK: OFS-7100 §12 deposit gate, enforced by *proof of
    /// non-existence*. Unchecked and uninitialized on purpose — the
    /// wallet is banned iff this address is occupied, so in the passing
    /// case there is nothing to deserialize. The soundness lives in the
    /// constraint, not the type: `seeds`/`seeds::program` force this to
    /// be the one canonical ban address for `buyer` under
    /// `openfiat-governance`, so a banned caller cannot substitute an
    /// unrelated empty account and appear unbanned. Removing either line
    /// silently disables the ban for this instruction.
    #[account(
        seeds = [openfiat_programs_shared::BAN_SEED, buyer.key().as_ref()],
        bump,
        seeds::program = openfiat_programs_shared::GOVERNANCE_PROGRAM_ID,
    )]
    pub ban_record: UncheckedAccount<'info>,

    #[account(
        mut,
        seeds = [SALE_CONFIG_SEED, &sale_nonce.to_le_bytes()],
        bump = sale_config.bump,
        constraint = sale_config.usdc_vault == usdc_vault.key(),
    )]
    pub sale_config: Account<'info, SaleConfig>,

    #[account(
        mut,
        constraint = buyer_usdc.owner == buyer.key(),
        constraint = buyer_usdc.mint == sale_config.usdc_mint,
    )]
    pub buyer_usdc: InterfaceAccount<'info, TokenAccount>,

    #[account(mut)]
    pub usdc_vault: InterfaceAccount<'info, TokenAccount>,

    #[account(
        constraint = usdc_mint.key() == sale_config.usdc_mint,
        mint::token_program = token_program,
    )]
    pub usdc_mint: InterfaceAccount<'info, Mint>,

    #[account(
        init_if_needed,
        payer = buyer,
        space = 8 + Contribution::INIT_SPACE,
        seeds = [CONTRIBUTION_SEED, sale_config.key().as_ref(), buyer.key().as_ref()],
        bump
    )]
    pub contribution: Account<'info, Contribution>,

    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

pub fn handle_contribute_usdc(
    ctx: Context<ContributeUsdc>,
    _sale_nonce: u64,
    amount: u64,
) -> Result<()> {
    require!(
        !openfiat_programs_shared::wallet_is_banned(&ctx.accounts.ban_record),
        ErrorCode::WalletBanned
    );

    let now = Clock::get()?.unix_timestamp;
    let sale_config = &ctx.accounts.sale_config;

    require!(
        sale_config.state == SaleState::Active,
        ErrorCode::SaleNotActive
    );
    require!(now >= sale_config.start_time, ErrorCode::SaleNotStarted);
    require!(now <= sale_config.end_time, ErrorCode::SaleEnded);

    let contribution = &ctx.accounts.contribution;
    let is_first_contribution = contribution.amount_usdc == 0;
    if is_first_contribution {
        require!(
            amount >= sale_config.min_contribution,
            ErrorCode::BelowMinimumContribution
        );
    }
    let new_wallet_total = contribution
        .amount_usdc
        .checked_add(amount)
        .ok_or(ErrorCode::Overflow)?;
    require!(
        new_wallet_total <= sale_config.max_contribution,
        ErrorCode::AboveMaximumContribution
    );
    let new_total_raised = sale_config
        .total_raised
        .checked_add(amount)
        .ok_or(ErrorCode::Overflow)?;
    require!(
        new_total_raised <= sale_config.hard_cap,
        ErrorCode::HardCapExceeded
    );

    transfer_checked(
        CpiContext::new(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: ctx.accounts.buyer_usdc.to_account_info(),
                mint: ctx.accounts.usdc_mint.to_account_info(),
                to: ctx.accounts.usdc_vault.to_account_info(),
                authority: ctx.accounts.buyer.to_account_info(),
            },
        ),
        amount,
        sale_config.usdc_decimals,
    )?;

    let open_delta = sale_config.open_entitlement_for(amount)?;

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
