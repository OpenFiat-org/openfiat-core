pub mod constants;
pub mod error;
pub mod instructions;
pub mod state;

use anchor_lang::prelude::*;

pub use constants::*;
pub use instructions::*;
pub use state::*;

declare_id!("HaPpM1QYM3dKp3sX7zhEdft9hB6ncu6xfALAbkyQChQP");

/// `openfiat-escrow` — Liquidity Vault + Trade Escrow Vault custody
/// (OFS-4200 §4). Phase 4: full vault lifecycle — create, deposit,
/// reserve, withdraw, create/fund a trade escrow, approve, release,
/// cancel, expire, freeze. Phase 4b adds the dispute-to-chain bridge
/// (`commit_dispute_vote`/`reveal_dispute_vote`/`execute_dispute_outcome`).
#[program]
pub mod escrow {
    use super::*;

    pub fn initialize_fee_config(
        ctx: Context<InitializeFeeConfig>,
        params: InitializeFeeConfigParams,
    ) -> Result<()> {
        crate::instructions::initialize_fee_config::handle_initialize_fee_config(ctx, params)
    }

    pub fn create_liquidity_vault(ctx: Context<CreateLiquidityVault>) -> Result<()> {
        crate::instructions::create_liquidity_vault::handle_create_liquidity_vault(ctx)
    }

    pub fn deposit_liquidity(ctx: Context<DepositLiquidity>, amount: u64) -> Result<()> {
        crate::instructions::deposit_liquidity::handle_deposit_liquidity(ctx, amount)
    }

    pub fn reserve_liquidity(ctx: Context<ReserveLiquidity>, amount: u64) -> Result<()> {
        crate::instructions::reserve_liquidity::handle_reserve_liquidity(ctx, amount)
    }

    pub fn withdraw_liquidity(ctx: Context<WithdrawLiquidity>, amount: u64) -> Result<()> {
        crate::instructions::withdraw_liquidity::handle_withdraw_liquidity(ctx, amount)
    }

    pub fn create_trade_escrow(
        ctx: Context<CreateTradeEscrow>,
        reservation_id: u64,
        amount: u64,
        dispute_authority: Pubkey,
        timeout_secs: i64,
    ) -> Result<()> {
        crate::instructions::create_trade_escrow::handle_create_trade_escrow(
            ctx,
            reservation_id,
            amount,
            dispute_authority,
            timeout_secs,
        )
    }

    pub fn fund_trade_escrow(ctx: Context<FundTradeEscrow>) -> Result<()> {
        crate::instructions::fund_trade_escrow::handle_fund_trade_escrow(ctx)
    }

    pub fn approve_settlement(ctx: Context<ApproveSettlement>) -> Result<()> {
        crate::instructions::approve_settlement::handle_approve_settlement(ctx)
    }

    pub fn release_escrow(ctx: Context<ReleaseEscrow>) -> Result<()> {
        crate::instructions::release_escrow::handle_release_escrow(ctx)
    }

    pub fn cancel_reservation(ctx: Context<CancelReservation>) -> Result<()> {
        crate::instructions::cancel_reservation::handle_cancel_reservation(ctx)
    }

    pub fn expire_reservation(ctx: Context<ExpireReservation>) -> Result<()> {
        crate::instructions::expire_reservation::handle_expire_reservation(ctx)
    }

    pub fn freeze_on_dispute(ctx: Context<FreezeOnDispute>) -> Result<()> {
        crate::instructions::freeze_on_dispute::handle_freeze_on_dispute(ctx)
    }
}
