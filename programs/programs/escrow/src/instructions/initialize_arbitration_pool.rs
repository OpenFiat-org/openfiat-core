use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, TokenAccount, TokenInterface};

use crate::{constants::*, error::ErrorCode, state::*};

/// Creates the singleton arbitration pool token account.
///
/// Separate from `initialize_fee_config` only because that instruction has
/// already run on devnet and a singleton cannot be re-initialised. Grouping
/// them would have been tidier on a fresh deployment; on a live one it
/// would mean abandoning the existing `FeeConfig`, and with it the
/// treasuries and fee parameters already pointing at it.
///
/// Admin-gated, unlike `staking::fund_rewards_vault`. That one only ever
/// increases a pool, so anyone may call it; this one *creates* the account
/// every later deposit and payout is pinned to, so the wrong mint here
/// would be permanent.
#[derive(Accounts)]
pub struct InitializeArbitrationPool<'info> {
    #[account(mut, constraint = admin.key() == fee_config.admin @ ErrorCode::Unauthorized)]
    pub admin: Signer<'info>,

    #[account(seeds = [FEE_CONFIG_SEED], bump = fee_config.bump)]
    pub fee_config: Account<'info, FeeConfig>,

    /// The OPEN mint. Arbitration deposits are OPEN-denominated
    /// (OFS-4100 §6), not settlement-stablecoin denominated.
    #[account(mint::token_program = token_program)]
    pub mint: InterfaceAccount<'info, Mint>,

    /// Authority is the `FeeConfig` PDA, so only this program can move
    /// what the pool holds — deposits in it are owed either back to a
    /// merchant or forward to arbitrators, never to the admin.
    #[account(
        init,
        payer = admin,
        seeds = [ARBITRATION_POOL_SEED],
        bump,
        token::mint = mint,
        token::authority = fee_config,
        token::token_program = token_program,
    )]
    pub arbitration_pool: InterfaceAccount<'info, TokenAccount>,

    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

pub fn handle_initialize_arbitration_pool(_ctx: Context<InitializeArbitrationPool>) -> Result<()> {
    Ok(())
}
