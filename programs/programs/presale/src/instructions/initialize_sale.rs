use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, Token2022, TokenAccount};

use crate::{constants::*, error::ErrorCode, state::*};

#[derive(Accounts)]
#[instruction(sale_nonce: u64)]
pub struct InitializeSale<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,

    /// `sale_nonce` namespaces this sale's PDAs so a new round can be
    /// initialized without redeploying (v1 production usage is a single
    /// sale at nonce 0; the nonce exists so this doesn't have to be a hard
    /// global singleton).
    #[account(
        init,
        payer = admin,
        space = 8 + SaleConfig::INIT_SPACE,
        seeds = [SALE_CONFIG_SEED, &sale_nonce.to_le_bytes()],
        bump
    )]
    pub sale_config: Account<'info, SaleConfig>,

    pub open_mint: InterfaceAccount<'info, Mint>,
    pub usdc_mint: InterfaceAccount<'info, Mint>,

    /// CHECK: the `presale_vault` PDA — verified via seeds/bump below and
    /// used only as the expected owner of `presale_vault`. It signs
    /// `claim`'s OPEN transfer later; never read/written here.
    #[account(seeds = [PRESALE_VAULT_SEED], bump)]
    pub presale_vault_authority: UncheckedAccount<'info>,

    /// The Community Presale allocation bucket, already funded at genesis
    /// (Phase 2) — verified here, not created here.
    #[account(
        constraint = presale_vault.mint == open_mint.key(),
        constraint = presale_vault.owner == presale_vault_authority.key() @ ErrorCode::Unauthorized,
    )]
    pub presale_vault: InterfaceAccount<'info, TokenAccount>,

    #[account(
        init,
        payer = admin,
        seeds = [SALE_USDC_VAULT_SEED, &sale_nonce.to_le_bytes()],
        bump,
        token::mint = usdc_mint,
        token::authority = sale_config,
        token::token_program = token_program,
    )]
    pub usdc_vault: InterfaceAccount<'info, TokenAccount>,

    /// Destination for collected USDC once finalized (e.g. a treasury
    /// multisig's ATA). Not created or owned by this program.
    #[account(constraint = treasury.mint == usdc_mint.key())]
    pub treasury: InterfaceAccount<'info, TokenAccount>,

    /// CHECK: the trusted swap-aggregator program id, recorded verbatim into
    /// `sale_config.swap_program` — see `contribute_with_swap` for why this
    /// program isn't invoked or validated further here. Production
    /// devnet/mainnet deployments must pass Jupiter's real, independently
    /// verified aggregator program id; test/CI deployments may pass a
    /// deterministic mock instead.
    pub swap_program: UncheckedAccount<'info>,

    pub token_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

/// Bundled instead of flattened so the generated `#[program]` dispatch entry
/// point stays under clippy's too-many-arguments threshold, and so a future
/// added field doesn't ripple through every caller's positional arg list.
#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InitializeSaleParams {
    pub hard_cap: u64,
    pub soft_cap: u64,
    pub min_contribution: u64,
    pub max_contribution: u64,
    pub max_slippage_bps: u16,
    pub start_time: i64,
    pub end_time: i64,
    pub stablecoin_whitelist: Vec<Pubkey>,
}

pub fn handle_initialize_sale(
    ctx: Context<InitializeSale>,
    _sale_nonce: u64,
    params: InitializeSaleParams,
) -> Result<()> {
    require!(
        params.stablecoin_whitelist.len() <= MAX_STABLECOINS,
        ErrorCode::WhitelistTooLong
    );
    require!(
        params.hard_cap > params.soft_cap,
        ErrorCode::HardCapNotGreaterThanSoftCap
    );
    require!(
        params.min_contribution > 0 && params.min_contribution <= params.max_contribution,
        ErrorCode::InvalidContributionBounds
    );
    require!(
        params.end_time > params.start_time,
        ErrorCode::InvalidSaleWindow
    );
    require!(
        params.max_slippage_bps > 0 && (params.max_slippage_bps as u64) <= BPS_DENOMINATOR,
        ErrorCode::InvalidSlippageBps
    );
    require!(
        ctx.accounts.open_mint.decimals >= ctx.accounts.usdc_mint.decimals,
        ErrorCode::Overflow
    );

    let sale_config = &mut ctx.accounts.sale_config;
    sale_config.admin = ctx.accounts.admin.key();
    sale_config.open_mint = ctx.accounts.open_mint.key();
    sale_config.usdc_mint = ctx.accounts.usdc_mint.key();
    sale_config.presale_vault = ctx.accounts.presale_vault.key();
    sale_config.usdc_vault = ctx.accounts.usdc_vault.key();
    sale_config.treasury = ctx.accounts.treasury.key();
    sale_config.swap_program = ctx.accounts.swap_program.key();
    sale_config.hard_cap = params.hard_cap;
    sale_config.soft_cap = params.soft_cap;
    sale_config.min_contribution = params.min_contribution;
    sale_config.max_contribution = params.max_contribution;
    sale_config.max_slippage_bps = params.max_slippage_bps;
    sale_config.open_decimals = ctx.accounts.open_mint.decimals;
    sale_config.usdc_decimals = ctx.accounts.usdc_mint.decimals;
    sale_config.start_time = params.start_time;
    sale_config.end_time = params.end_time;
    sale_config.stablecoin_whitelist = params.stablecoin_whitelist;
    sale_config.total_raised = 0;
    sale_config.state = SaleState::Active;
    sale_config.bump = ctx.bumps.sale_config;
    sale_config.usdc_vault_bump = ctx.bumps.usdc_vault;
    Ok(())
}
