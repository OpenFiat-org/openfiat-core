use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    transfer_checked, Mint, Token2022, TokenAccount, TransferChecked,
};

use crate::{constants::*, error::ErrorCode, events::SlashApplied, state::*};

/// Callable only by `slashing_authority` (OFS-4200 §5). Applies to the
/// stake account's active `amount` only — not `unbonding_amount`, which
/// is already leaving the system on its own timer.
///
/// A slash may leave the balance below the role's minimum. That is
/// deliberate: keeping the penalty at `slash_bps` matters more than
/// keeping every balance above the floor, because sweeping the remainder
/// to zero would turn a moderate penalty into total forfeiture for anyone
/// staked at the minimum — the common case, and contrary to OFS-2400
/// §16's "partial, moderate stake slash". The resulting account holds
/// tokens but confers no weight; see [`StakeAccount::effective_stake`],
/// which is where that is enforced for every reader at once.
///
/// `misconduct_code` is recorded in the emitted [`SlashApplied`] event.
/// It was previously discarded outright — the parameter was `_`-prefixed
/// — so a slash left no on-chain trace of why it happened. The off-chain
/// `disputes` relay (OFS-4200 §1) remains the source of truth for the
/// underlying evidence; this is the on-chain breadcrumb pointing at it.
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

pub fn handle_slash(ctx: Context<Slash>, misconduct_code: u16) -> Result<()> {
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

    let min_stake = ctx
        .accounts
        .staking_config
        .min_stake_for(ctx.accounts.stake_account.role);

    let stake_account = &mut ctx.accounts.stake_account;
    stake_account.amount = stake_account
        .amount
        .checked_sub(slash_amount)
        .ok_or(ErrorCode::Overflow)?;
    stake_account.slashed_total = stake_account
        .slashed_total
        .checked_add(slash_amount)
        .ok_or(ErrorCode::Overflow)?;
    // Keeps `first_staked_at`'s "zero balance means zero clock" invariant
    // true through the one path that can empty an account without its
    // owner asking. A partial slash deliberately leaves the clock running:
    // whether misconduct should also cost an arbitrator their accrued age
    // is a policy question OFS-4100 has not decided, and quietly deciding
    // it here would make a slash harsher than §16's "partial, moderate"
    // without anyone signing that off.
    if stake_account.amount == 0 {
        stake_account.first_staked_at = 0;
    }

    emit!(SlashApplied {
        stake_account: stake_account.key(),
        owner: stake_account.owner,
        role: stake_account.role,
        misconduct_code,
        amount: slash_amount,
        remaining_stake: stake_account.amount,
        slashed_total: stake_account.slashed_total,
        eligible_after: stake_account.amount >= min_stake,
        destination: ctx.accounts.destination.key(),
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}
