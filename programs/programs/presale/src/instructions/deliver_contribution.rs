use anchor_lang::prelude::*;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token_interface::{
    transfer_checked, Mint, TokenAccount, TokenInterface, TransferChecked,
};

use crate::{constants::*, error::ErrorCode, state::*};

/// Emitted once per successful cross-chain delivery — the deBridge-side
/// analogue of a direct `contribute_usdc` call, except OPEN lands in the
/// same transaction instead of requiring a separate `claim`.
#[event]
pub struct ContributionDelivered {
    pub recipient: Pubkey,
    pub usdc_amount: u64,
    pub open_delivered: u64,
    pub total_raised: u64,
}

/// Credits + auto-delivers OPEN for a cross-chain buyer in one instruction,
/// called by a deBridge Solana Hook after the DLN has already landed
/// `usdc_amount` USDC in `source_usdc` (OFS-4100 §3 cross-chain presale
/// extension — SP-B).
///
/// # Binding: `payer` funds, `recipient` receives
///
/// The deBridge executor (`payer`) signs and pays every rent (the
/// `Contribution` PDA, and `recipient_open` via `init_if_needed`), but
/// `recipient` — a non-signer `Pubkey` carried in the instruction args —
/// is what the `Contribution` PDA is seeded from and what `recipient_open`
/// is constrained to be owned by. A malicious or buggy `payer` can spend
/// its own SOL funding these accounts, but cannot redirect the delivered
/// OPEN anywhere but `recipient`'s own ATA.
///
/// # No free mint
///
/// OPEN is credited only against the *measured* `usdc_vault` balance delta
/// produced by this instruction's own `transfer_checked` — read before the
/// CPI, `reload()`d after, subtracted. This mirrors `contribute_with_swap`'s
/// proven pattern and is deliberately not a cumulative
/// `usdc_vault.amount >= total_raised` check: `sweep_proceeds` moves USDC
/// out of the vault mid-sale, so a cumulative check would either falsely
/// reject deliveries after a sweep or (worse) be satisfied without this
/// instruction's own transfer ever landing.
#[derive(Accounts)]
#[instruction(sale_nonce: u64, recipient: Pubkey)]
pub struct DeliverContribution<'info> {
    /// The deBridge executor filling the order — signs and pays all rents.
    #[account(mut)]
    pub payer: Signer<'info>,

    /// CHECK: OFS-7100 §12 ban gate for the *recipient* (proof-of-non-existence),
    /// exactly as in contribute_usdc — see that instruction's doc comment for
    /// why the soundness lives in the constraints, not the type.
    #[account(
        seeds = [openfiat_programs_shared::BAN_SEED, recipient.as_ref()],
        bump,
        seeds::program = openfiat_programs_shared::GOVERNANCE_PROGRAM_ID,
    )]
    pub ban_record: UncheckedAccount<'info>,

    #[account(
        mut,
        seeds = [SALE_CONFIG_SEED, &sale_nonce.to_le_bytes()],
        bump = sale_config.bump,
        constraint = sale_config.usdc_vault == usdc_vault.key(),
        constraint = sale_config.presale_vault == presale_vault.key(),
        constraint = sale_config.open_mint == open_mint.key(),
        constraint = sale_config.usdc_mint == usdc_mint.key(),
    )]
    pub sale_config: Box<Account<'info, SaleConfig>>,

    /// The USDC mint — needed as `transfer_checked`'s own mint account for
    /// the delivered-USDC leg; distinct from `open_mint` and may move under
    /// a different token program.
    #[account(mint::token_program = usdc_token_program)]
    pub usdc_mint: Box<InterfaceAccount<'info, Mint>>,

    /// USDC the DLN delivered, authorized by `payer`.
    #[account(mut)]
    pub source_usdc: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub usdc_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mint::token_program = open_token_program)]
    pub open_mint: Box<InterfaceAccount<'info, Mint>>,
    /// CHECK: PDA that signs the OPEN transfer (seeds/bump).
    #[account(seeds = [PRESALE_VAULT_SEED], bump)]
    pub presale_vault_authority: UncheckedAccount<'info>,
    #[account(mut)]
    pub presale_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    /// CHECK: the bound recipient; only used as an ATA owner + Contribution
    /// seed. Equality against the `recipient` instruction arg is enforced
    /// below so a caller cannot pass an unrelated account here.
    #[account(constraint = recipient_account.key() == recipient)]
    pub recipient_account: UncheckedAccount<'info>,
    #[account(
        init_if_needed,
        payer = payer,
        associated_token::mint = open_mint,
        associated_token::authority = recipient_account,
        associated_token::token_program = open_token_program,
    )]
    pub recipient_open: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        init_if_needed,
        payer = payer,
        space = 8 + Contribution::INIT_SPACE,
        seeds = [CONTRIBUTION_SEED, sale_config.key().as_ref(), recipient.as_ref()],
        bump
    )]
    pub contribution: Box<Account<'info, Contribution>>,

    /// USDC is moved with the source's token program; OPEN with the mint's
    /// — the two can differ (e.g. one Token-2022, one legacy SPL Token).
    pub usdc_token_program: Interface<'info, TokenInterface>,
    pub open_token_program: Interface<'info, TokenInterface>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

pub fn handle_deliver_contribution(
    ctx: Context<DeliverContribution>,
    _sale_nonce: u64,
    recipient: Pubkey,
    usdc_amount: u64,
) -> Result<()> {
    require!(
        !openfiat_programs_shared::wallet_is_banned(&ctx.accounts.ban_record),
        ErrorCode::WalletBanned
    );

    let now = Clock::get()?.unix_timestamp;
    {
        let sc = &ctx.accounts.sale_config;
        require!(sc.state == SaleState::Active, ErrorCode::SaleNotActive);
        require!(now >= sc.start_time, ErrorCode::SaleNotStarted);
        require!(now <= sc.end_time, ErrorCode::SaleEnded);
    }

    // Pull the delivered USDC into the vault, measuring the real delta —
    // never trust `usdc_amount` alone (see the module doc comment's
    // no-free-mint note).
    let usdc_before = ctx.accounts.usdc_vault.amount;
    let usdc_decimals = ctx.accounts.sale_config.usdc_decimals;
    transfer_checked(
        CpiContext::new(
            ctx.accounts.usdc_token_program.key(),
            TransferChecked {
                from: ctx.accounts.source_usdc.to_account_info(),
                mint: ctx.accounts.usdc_mint.to_account_info(),
                to: ctx.accounts.usdc_vault.to_account_info(),
                authority: ctx.accounts.payer.to_account_info(),
            },
        ),
        usdc_amount,
        usdc_decimals,
    )?;
    ctx.accounts.usdc_vault.reload()?;
    let delta = ctx
        .accounts
        .usdc_vault
        .amount
        .checked_sub(usdc_before)
        .ok_or(ErrorCode::Overflow)?;
    require!(delta == usdc_amount, ErrorCode::UsdcDeliveryMismatch);

    // Credit exactly like contribute_with_swap (min/max/hard_cap).
    let sc = &ctx.accounts.sale_config;
    let first = ctx.accounts.contribution.amount_usdc == 0;
    if first {
        require!(
            delta >= sc.min_contribution,
            ErrorCode::BelowMinimumContribution
        );
    }
    let new_wallet_total = ctx
        .accounts
        .contribution
        .amount_usdc
        .checked_add(delta)
        .ok_or(ErrorCode::Overflow)?;
    require!(
        new_wallet_total <= sc.max_contribution,
        ErrorCode::AboveMaximumContribution
    );
    let new_total_raised = sc
        .total_raised
        .checked_add(delta)
        .ok_or(ErrorCode::Overflow)?;
    require!(new_total_raised <= sc.hard_cap, ErrorCode::HardCapExceeded);
    let open_out = sc.open_entitlement_for(delta)?;
    let open_decimals = sc.open_decimals;

    // Auto-deliver OPEN, PDA-signed (mirrors claim.rs).
    let bump = ctx.bumps.presale_vault_authority;
    let seeds: &[&[u8]] = &[PRESALE_VAULT_SEED, &[bump]];
    transfer_checked(
        CpiContext::new_with_signer(
            ctx.accounts.open_token_program.key(),
            TransferChecked {
                from: ctx.accounts.presale_vault.to_account_info(),
                mint: ctx.accounts.open_mint.to_account_info(),
                to: ctx.accounts.recipient_open.to_account_info(),
                authority: ctx.accounts.presale_vault_authority.to_account_info(),
            },
            &[seeds],
        ),
        open_out,
        open_decimals,
    )?;

    let c = &mut ctx.accounts.contribution;
    c.buyer = recipient;
    c.amount_usdc = new_wallet_total;
    c.open_entitlement = c
        .open_entitlement
        .checked_add(open_out)
        .ok_or(ErrorCode::Overflow)?;
    // Delivered now, in this same instruction — nothing left to `claim`.
    c.claimed_open = c
        .claimed_open
        .checked_add(open_out)
        .ok_or(ErrorCode::Overflow)?;
    c.bump = ctx.bumps.contribution;
    ctx.accounts.sale_config.total_raised = new_total_raised;

    emit!(ContributionDelivered {
        recipient,
        usdc_amount: delta,
        open_delivered: open_out,
        total_raised: new_total_raised,
    });
    Ok(())
}
