use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    transfer_checked, Mint, Token2022, TokenAccount, TransferChecked,
};

use crate::{constants::*, error::ErrorCode, events::RewardsVaultFunded, state::*};

/// Moves OPEN into the reward pool `claim_rewards` pays out of.
///
/// Nothing funded that vault before this existed. `initialize_staking_config`
/// creates it and `claim_rewards` draws from it, but no instruction ever
/// put anything in, so its balance was zero and every claim failed even
/// once the cranker had credited `pending_rewards`. Computing a reward and
/// paying one are different things, and only the first had a code path.
///
/// **Permissionless by design.** The obvious instinct is to gate this on
/// `admin`, but the only thing it can do is *increase* a pool that pays
/// stakers, and refusing a donation protects nobody. OFS-4100 §9.1 names
/// two funders — the Infrastructure genesis bucket as a finite bootstrap
/// and the Infrastructure treasury's share of settlement fees as the
/// steady state — and neither is this program's `admin`; requiring admin
/// would mean re-issuing authority every time a funding source changed.
/// Draining stays gated: tokens leave only via `claim_rewards`, against a
/// `pending_rewards` balance only `rewards_authority` can create.
///
/// The vault is an ordinary token account, so a plain SPL transfer could
/// always have reached it. That is precisely the problem this fixes: such
/// a transfer leaves no protocol-level record. Going through an
/// instruction emits [`RewardsVaultFunded`], so the pool's funding history
/// is reconstructable from logs (§9.4) instead of being inferable only by
/// diffing balances.
#[derive(Accounts)]
pub struct FundRewardsVault<'info> {
    pub funder: Signer<'info>,

    pub mint: InterfaceAccount<'info, Mint>,

    #[account(
        seeds = [STAKING_CONFIG_SEED],
        bump = staking_config.bump,
        constraint = staking_config.mint == mint.key(),
    )]
    pub staking_config: Account<'info, StakingConfig>,

    #[account(mut, seeds = [REWARDS_VAULT_SEED], bump = staking_config.rewards_vault_bump)]
    pub rewards_vault: InterfaceAccount<'info, TokenAccount>,

    #[account(mut, constraint = from.mint == mint.key())]
    pub from: InterfaceAccount<'info, TokenAccount>,

    pub token_program: Program<'info, Token2022>,
}

pub fn handle_fund_rewards_vault(ctx: Context<FundRewardsVault>, amount: u64) -> Result<()> {
    require!(amount > 0, ErrorCode::ZeroAmount);

    transfer_checked(
        CpiContext::new(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: ctx.accounts.from.to_account_info(),
                mint: ctx.accounts.mint.to_account_info(),
                to: ctx.accounts.rewards_vault.to_account_info(),
                authority: ctx.accounts.funder.to_account_info(),
            },
        ),
        amount,
        ctx.accounts.mint.decimals,
    )?;

    // Re-read rather than adding to the cached figure: the transfer above
    // just mutated this account, and the deserialized copy predates it.
    ctx.accounts.rewards_vault.reload()?;

    emit!(RewardsVaultFunded {
        funder: ctx.accounts.funder.key(),
        amount,
        vault_balance: ctx.accounts.rewards_vault.amount,
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}
