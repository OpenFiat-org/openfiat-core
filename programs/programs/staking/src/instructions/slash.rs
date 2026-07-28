use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    transfer_checked, Mint, Token2022, TokenAccount, TransferChecked,
};

use crate::{constants::*, error::ErrorCode, state::*};

/// Callable only by `slashing_authority` (OFS-4200 §5). Applies to the
/// stake account's active `amount` only — not `unbonding_amount`, which
/// is already leaving the system on its own timer. `misconduct_code` is
/// recorded nowhere on-chain beyond this instruction's own logged
/// arguments; the caller (the off-chain `disputes` crate's relay, for
/// arbitrator misconduct specifically, per OFS-4200 §1) is the source of
/// truth for *why* a slash happened.
#[derive(Accounts)]
pub struct Slash<'info> {
    #[account(constraint = slashing_authority.key() == staking_config.slashing_authority @ ErrorCode::NotSlashingAuthority)]
    pub slashing_authority: Signer<'info>,

    pub mint: InterfaceAccount<'info, Mint>,

    #[account(
        seeds = [STAKING_CONFIG_SEED],
        bump = staking_config.bump,
        constraint = staking_config.mint == mint.key(),
    )]
    pub staking_config: Account<'info, StakingConfig>,

    #[account(
        mut,
        seeds = [STAKE_ACCOUNT_SEED, stake_account.owner.as_ref(), &[stake_account.role as u8]],
        bump = stake_account.bump,
    )]
    pub stake_account: Account<'info, StakeAccount>,

    #[account(mut, seeds = [STAKE_VAULT_SEED], bump = staking_config.stake_vault_bump)]
    pub stake_vault: InterfaceAccount<'info, TokenAccount>,

    #[account(mut, constraint = destination.key() == staking_config.slash_destination)]
    pub destination: InterfaceAccount<'info, TokenAccount>,

    pub token_program: Program<'info, Token2022>,
}

pub fn handle_slash(ctx: Context<Slash>, _misconduct_code: u16) -> Result<()> {
    let slash_bps = ctx.accounts.staking_config.slash_bps;
    let stake_account = &ctx.accounts.stake_account;
    let slash_amount = (stake_account.amount as u128)
        .checked_mul(slash_bps as u128)
        .ok_or(ErrorCode::Overflow)?
        .checked_div(BPS_DENOMINATOR as u128)
        .ok_or(ErrorCode::Overflow)? as u64;

    if slash_amount > 0 {
        let bump = ctx.accounts.staking_config.bump;
        let signer_seeds: &[&[u8]] = &[STAKING_CONFIG_SEED, &[bump]];
        transfer_checked(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.key(),
                TransferChecked {
                    from: ctx.accounts.stake_vault.to_account_info(),
                    mint: ctx.accounts.mint.to_account_info(),
                    to: ctx.accounts.destination.to_account_info(),
                    authority: ctx.accounts.staking_config.to_account_info(),
                },
                &[signer_seeds],
            ),
            slash_amount,
            ctx.accounts.mint.decimals,
        )?;
    }

    let stake_account = &mut ctx.accounts.stake_account;
    stake_account.amount -= slash_amount;
    stake_account.slashed_total = stake_account
        .slashed_total
        .checked_add(slash_amount)
        .ok_or(ErrorCode::Overflow)?;
    Ok(())
}
