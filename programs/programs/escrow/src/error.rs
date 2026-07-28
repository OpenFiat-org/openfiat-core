use anchor_lang::prelude::*;

#[error_code]
pub enum ErrorCode {
    #[msg("Only the fee config admin may perform this action")]
    Unauthorized,
    #[msg("Fee treasury split basis points must sum to exactly 10_000")]
    InvalidFeeSplit,
    #[msg("settlement_fee_bps must be between 0 and 10_000")]
    InvalidFeeBps,
    #[msg("timeout_secs must be greater than zero")]
    InvalidTimeout,
    #[msg("Requested amount exceeds this vault's available balance")]
    InsufficientAvailableLiquidity,
    #[msg("Requested amount exceeds this vault's reserved balance")]
    InsufficientReservedLiquidity,
    #[msg("This trade escrow is not in the expected state for this action")]
    InvalidVaultState,
    #[msg("This trade escrow's payment/review window has not yet expired")]
    NotYetExpired,
    #[msg("Only the buyer or seller on this trade escrow may perform this action")]
    NotAPartyToThisTrade,
    #[msg("Only this trade escrow's configured dispute authority may perform this action")]
    NotDisputeAuthority,
    #[msg("Arithmetic overflow")]
    Overflow,
}
