use anchor_lang::prelude::*;

#[error_code]
pub enum ErrorCode {
    #[msg("Only the sale admin may perform this action")]
    Unauthorized,
    #[msg("Stablecoin whitelist may not exceed MAX_STABLECOINS entries")]
    WhitelistTooLong,
    #[msg("hard_cap must be greater than soft_cap")]
    HardCapNotGreaterThanSoftCap,
    #[msg("min_contribution must be greater than zero and at most max_contribution")]
    InvalidContributionBounds,
    #[msg("end_time must be after start_time")]
    InvalidSaleWindow,
    #[msg("max_slippage_bps must be between 1 and 10_000")]
    InvalidSlippageBps,
    #[msg("The sale has not started yet")]
    SaleNotStarted,
    #[msg("The sale has already ended")]
    SaleEnded,
    #[msg("The sale is not in the Active state")]
    SaleNotActive,
    #[msg("This asset is not on the presale's accepted-stablecoin whitelist")]
    AssetNotWhitelisted,
    #[msg("Use contribute_usdc for direct USDC contributions instead")]
    UseDirectUsdcPath,
    #[msg(
        "This contribution would be below the minimum required for a wallet's first contribution"
    )]
    BelowMinimumContribution,
    #[msg("This contribution would exceed the maximum allowed per wallet")]
    AboveMaximumContribution,
    #[msg("This contribution would exceed the sale's hard cap")]
    HardCapExceeded,
    #[msg("The swap program account does not match sale_config.swap_program")]
    SwapProgramMismatch,
    #[msg("The swap's actual USDC output was below the required minimum (slippage exceeded)")]
    SlippageExceeded,
    #[msg("The sale has not ended yet")]
    SaleNotEnded,
    #[msg("The sale has already been finalized or resolved")]
    SaleAlreadyResolved,
    #[msg("Claims are only allowed after the sale has been finalized")]
    SaleNotFinalized,
    #[msg("This contribution has already been claimed")]
    AlreadyClaimed,
    #[msg("Refunds are only allowed when the sale's soft cap was missed")]
    SaleNotRefundable,
    #[msg("This contribution has already been refunded")]
    AlreadyRefunded,
    #[msg("Arithmetic overflow")]
    Overflow,
    // Appended, never inserted: Anchor numbers error codes by
    // declaration order, so adding a variant above an existing one
    // renumbers every code after it and breaks clients matching on the
    // old number.
    #[msg("This wallet is on the governance ban list (OFS-7100 §12)")]
    WalletBanned,
    #[msg("soft_cap must be zero: this sale has no refund path, so a non-zero soft cap is unsupported")]
    SoftCapNotSupported,
    #[msg("Nothing to claim: your full OPEN entitlement has already been claimed")]
    NothingToClaim,
}
