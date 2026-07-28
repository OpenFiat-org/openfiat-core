use anchor_lang::prelude::*;
use openfiat_programs_shared::VaultState;

use crate::{constants::*, error::ErrorCode, state::*};

/// OFS-2300 §15: "Merchant Approves -> Settlement Approved Event ->
/// Program Releases Escrow." This instruction records the approval;
/// `release_escrow` is the only instruction that actually moves funds.
#[derive(Accounts)]
pub struct ApproveSettlement<'info> {
    pub merchant: Signer<'info>,

    #[account(
        mut,
        seeds = [TRADE_ESCROW_SEED, &trade_escrow.reservation_id.to_le_bytes()],
        bump = trade_escrow.bump,
        constraint = trade_escrow.seller == merchant.key(),
        constraint = trade_escrow.state == VaultState::AwaitingFiatSettlement @ ErrorCode::InvalidVaultState,
    )]
    pub trade_escrow: Account<'info, TradeEscrowVault>,
}

pub fn handle_approve_settlement(ctx: Context<ApproveSettlement>) -> Result<()> {
    ctx.accounts.trade_escrow.approved = true;
    Ok(())
}
