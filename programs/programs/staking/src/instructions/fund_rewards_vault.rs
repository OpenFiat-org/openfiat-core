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
///
/// # Banned wallets are the one exception, and it was a close call
///
/// "Permissionless" above means no *authority* — it does not extend to a
/// wallet on OFS-7100 §12's ban list. That sits in real tension with the
/// paragraph above, so the reasoning is recorded rather than left to be
/// re-derived.
///
/// Against gating: this is a one-way donation. The funder receives no
/// stake account, no claim, no receipt, no position — the tokens leave
/// their control permanently. Refusing therefore denies a banned party
/// nothing they wanted, while costing the pool a contribution. On that
/// reading the gate punishes the protocol more than the listed wallet.
///
/// For gating, and decisive: accepting means knowingly taking tokens from
/// a wallet governance has declared stolen or sanctioned and distributing
/// them to honest stakers, who cannot refuse them and had no part in the
/// decision. Pushing that contamination onto uninvolved third parties is
/// worse than declining a donation nobody was counting on — realistically
/// this pool is funded by the two sources §9.1 names, not by walk-up
/// donors, so the expected cost of refusing is near zero. §12 also says
/// "any vault" without exception, and the rewards vault is a vault;
/// carving out the first exception invites the argument for the next.
///
/// What the gate is not: a laundering defence. A determined actor donates
/// from a fresh, unlisted wallet and this check never fires. What it buys
/// is narrower and still worth having — the protocol does not knowingly
/// accept from a wallet it has itself listed.
#[derive(Accounts)]
pub struct FundRewardsVault<'info> {
    pub funder: Signer<'info>,

    /// CHECK: OFS-7100 §12 deposit gate, enforced by *proof of
    /// non-existence*. Unchecked and uninitialized on purpose — the
    /// wallet is banned iff this address is occupied, so in the passing
    /// case there is nothing to deserialize. The soundness lives in the
    /// constraint, not the type: `seeds`/`seeds::program` force this to
    /// be the one canonical ban address for `funder` under
    /// `openfiat-governance`, so a banned caller cannot substitute an
    /// unrelated empty account and appear unbanned. Removing either line
    /// silently disables the ban for this instruction.
    #[account(
        seeds = [openfiat_programs_shared::BAN_SEED, funder.key().as_ref()],
        bump,
        seeds::program = openfiat_programs_shared::GOVERNANCE_PROGRAM_ID,
    )]
    pub ban_record: UncheckedAccount<'info>,

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
    require!(
        !openfiat_programs_shared::wallet_is_banned(&ctx.accounts.ban_record),
        ErrorCode::WalletBanned
    );

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
