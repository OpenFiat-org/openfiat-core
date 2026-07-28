use anchor_lang::prelude::*;
use openfiat_programs_shared::VaultState;

use crate::{constants::*, error::ErrorCode, state::*};

/// Settable only by this escrow's configured `dispute_authority` (OFS-4200
/// §4). Phase 4b (plan decision #2) replaces the external-signer model
/// here with this same program's own on-chain dispute-case tally logic.
#[derive(Accounts)]
pub struct FreezeOnDispute<'info> {
    #[account(constraint = dispute_authority.key() == trade_escrow.dispute_authority @ ErrorCode::NotDisputeAuthority)]
    pub dispute_authority: Signer<'info>,

    #[account(
        mut,
        seeds = [TRADE_ESCROW_SEED, &trade_escrow.reservation_id.to_le_bytes()],
        bump = trade_escrow.bump,
        constraint = trade_escrow.state == VaultState::AwaitingFiatSettlement @ ErrorCode::InvalidVaultState,
    )]
    pub trade_escrow: Account<'info, TradeEscrowVault>,
}

pub fn handle_freeze_on_dispute(ctx: Context<FreezeOnDispute>) -> Result<()> {
    ctx.accounts.trade_escrow.state = VaultState::Frozen;
    Ok(())
}
