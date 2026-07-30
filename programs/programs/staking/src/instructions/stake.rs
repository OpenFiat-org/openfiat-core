use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    transfer_checked, Mint, TokenAccount, TokenInterface, TransferChecked,
};

use crate::{constants::*, error::ErrorCode, state::*};

#[derive(Accounts)]
pub struct Stake<'info> {
    pub owner: Signer<'info>,

    /// CHECK: OFS-7100 §12 deposit gate, enforced by *proof of
    /// non-existence*. Unchecked and uninitialized on purpose — the
    /// wallet is banned iff this address is occupied, so in the passing
    /// case there is nothing to deserialize. The soundness lives in the
    /// constraint, not the type: `seeds`/`seeds::program` force this to
    /// be the one canonical ban address for `owner` under
    /// `openfiat-governance`, so a banned caller cannot substitute an
    /// unrelated empty account and appear unbanned. Removing either line
    /// silently disables the ban for this instruction.
    #[account(
        seeds = [openfiat_programs_shared::BAN_SEED, owner.key().as_ref()],
        bump,
        seeds::program = openfiat_programs_shared::GOVERNANCE_PROGRAM_ID,
    )]
    pub ban_record: UncheckedAccount<'info>,

    #[account(
        seeds = [STAKING_CONFIG_SEED],
        bump = staking_config.bump,
        constraint = staking_config.mint == mint.key(),
    )]
    pub staking_config: Account<'info, StakingConfig>,

    #[account(
        mut,
        seeds = [STAKE_ACCOUNT_SEED, owner.key().as_ref(), &[stake_account.role as u8]],
        bump = stake_account.bump,
        has_one = owner,
    )]
    pub stake_account: Account<'info, StakeAccount>,

    #[account(mut, seeds = [STAKE_VAULT_SEED], bump = staking_config.stake_vault_bump)]
    pub stake_vault: InterfaceAccount<'info, TokenAccount>,

    #[account(mut, constraint = from.mint == mint.key())]
    pub from: InterfaceAccount<'info, TokenAccount>,

    #[account(mint::token_program = token_program)]
    pub mint: InterfaceAccount<'info, Mint>,

    pub token_program: Interface<'info, TokenInterface>,
}

pub fn handle_stake(ctx: Context<Stake>, amount: u64) -> Result<()> {
    require!(
        !openfiat_programs_shared::wallet_is_banned(&ctx.accounts.ban_record),
        ErrorCode::WalletBanned
    );

    // Checked before the transfer, so a stake that would land below the
    // role's minimum fails without having moved any tokens.
    let new_amount = ctx
        .accounts
        .stake_account
        .amount
        .checked_add(amount)
        .ok_or(ErrorCode::Overflow)?;
    require!(
        ctx.accounts
            .staking_config
            .is_legal_balance(ctx.accounts.stake_account.role, new_amount),
        ErrorCode::StakeBelowRoleMinimum
    );

    transfer_checked(
        CpiContext::new(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: ctx.accounts.from.to_account_info(),
                mint: ctx.accounts.mint.to_account_info(),
                to: ctx.accounts.stake_vault.to_account_info(),
                authority: ctx.accounts.owner.to_account_info(),
            },
        ),
        amount,
        ctx.accounts.mint.decimals,
    )?;

    // Start the age clock only on the transition out of zero, so a top-up
    // never resets it — see `StakeAccount::first_staked_at`.
    if ctx.accounts.stake_account.amount == 0 && new_amount > 0 {
        ctx.accounts.stake_account.first_staked_at = Clock::get()?.unix_timestamp;
    }
    ctx.accounts.stake_account.amount = new_amount;
    Ok(())
}
