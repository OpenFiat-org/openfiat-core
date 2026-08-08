use anchor_lang::prelude::*;

use crate::{constants::*, error::ErrorCode, state::*};

/// Atomic counter marking only — no token movement (OFS-4200 §4).
///
/// Requires the merchant's own signature, matching every other
/// liquidity-vault-mutating instruction in this program: the off-chain
/// `reservations` crate (OFS-2200) already decides *whether* a given
/// buyer may reserve against a published ad's terms; this instruction
/// only executes that already-made decision on-chain, relayed by the
/// merchant's own node/wallet (the same "off-chain protocol decides,
/// on-chain program executes" split this workspace already uses for
/// chain-bridge transaction relay). A permissionless version would let
/// any caller lock up a merchant's advertised liquidity with no
/// corresponding real trade — a griefing vector this design avoids by
/// construction rather than by a runtime check.
#[derive(Accounts)]
pub struct ReserveLiquidity<'info> {
    pub merchant: Signer<'info>,

    /// CHECK: OFS-7100 §12 ban gate, enforced by proof of non-existence —
    /// banned iff this canonical PDA is occupied; unchecked/uninitialized on
    /// purpose. seeds/seeds::program force the one canonical ban address for
    /// `merchant`, so a banned caller cannot substitute an empty account.
    #[account(
        seeds = [openfiat_programs_shared::BAN_SEED, merchant.key().as_ref()],
        bump,
        seeds::program = openfiat_programs_shared::GOVERNANCE_PROGRAM_ID,
    )]
    pub ban_record: UncheckedAccount<'info>,

    #[account(
        mut,
        seeds = [LIQUIDITY_VAULT_SEED, merchant.key().as_ref(), liquidity_vault.mint.as_ref()],
        bump = liquidity_vault.bump,
        has_one = merchant,
    )]
    pub liquidity_vault: Account<'info, LiquidityVault>,

    /// Read for one thing only: whether this vault's mint is still on the
    /// settlement allowlist.
    ///
    /// This is what makes de-listing safe rather than destructive. A
    /// de-listed mint must not strand anybody's money, so the vault stays
    /// intact, `deposit_liquidity` and `withdraw_liquidity` keep working,
    /// and every already-open trade runs to completion. What stops is *new
    /// exposure*, and a reservation is where new exposure starts: it is the
    /// commitment a merchant advertises against, so refusing here means a
    /// de-listed mint quietly stops being offered instead of failing later
    /// in front of a buyer who has already agreed to trade.
    #[account(seeds = [FEE_CONFIG_SEED], bump = fee_config.bump)]
    pub fee_config: Box<Account<'info, FeeConfig>>,
}

pub fn handle_reserve_liquidity(ctx: Context<ReserveLiquidity>, amount: u64) -> Result<()> {
    require!(
        !openfiat_programs_shared::wallet_is_banned(&ctx.accounts.ban_record),
        ErrorCode::WalletBanned
    );

    require!(
        ctx.accounts
            .fee_config
            .allows_settlement_mint(&ctx.accounts.liquidity_vault.mint),
        ErrorCode::SettlementMintNotAllowed
    );

    let liquidity_vault = &mut ctx.accounts.liquidity_vault;
    require!(
        liquidity_vault.available >= amount,
        ErrorCode::InsufficientAvailableLiquidity
    );
    liquidity_vault.available -= amount;
    liquidity_vault.reserved = liquidity_vault
        .reserved
        .checked_add(amount)
        .ok_or(ErrorCode::Overflow)?;
    Ok(())
}
