use anchor_lang::prelude::*;

/// Placeholder for the `SaleConfig` singleton described in OFS-4200 §3.
///
/// Phase 1 scaffolding only — the real fields (hard/soft cap, min/max
/// contribution, stablecoin whitelist, slippage tolerance, sale window)
/// land in Phase 3 once OFS-4100's presale terms are signed off.
#[account]
#[derive(InitSpace)]
pub struct SaleConfig {
    pub admin: Pubkey,
    pub bump: u8,
}
