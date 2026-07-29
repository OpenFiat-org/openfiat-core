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
    #[msg("This dispute case's commit window has already closed")]
    CommitWindowClosed,
    #[msg("This dispute case's reveal window is not open (either still committing, or already closed)")]
    NotInRevealWindow,
    #[msg("This dispute case is already at maximum arbitrator capacity")]
    DisputeCaseFull,
    #[msg("This wallet has already committed a vote for this dispute case")]
    AlreadyCommitted,
    #[msg("This wallet has not committed a vote for this dispute case")]
    NoCommitmentFound,
    #[msg("This wallet has already revealed its vote for this dispute case")]
    AlreadyRevealed,
    #[msg("The revealed outcome and salt do not match the stored commitment")]
    CommitmentMismatch,
    #[msg("This dispute case's reveal window has not yet closed")]
    RevealWindowStillOpen,
    #[msg("This dispute case has already been resolved")]
    DisputeAlreadyResolved,
    #[msg("No arbitrator revealed a vote for this dispute case")]
    NoVotesRevealed,
    #[msg("An arbitrator must hold at least the Arbitrator role's minimum stake to commit a vote")]
    ArbitratorStakeBelowMinimum,
    #[msg("Dispute commit/reveal windows must be within the protocol's permitted range")]
    DisputeWindowOutOfRange,
    #[msg("The deposit vault and the settlement liquidity vault must be different accounts")]
    DepositVaultAliasesSettlementVault,
    #[msg("No fee is configured for this action")]
    NoFeeConfigured,
    #[msg("This dispute case has not been resolved with a decisive outcome")]
    DisputeNotDecided,
    #[msg("This wallet did not vote with the winning outcome on this dispute case")]
    NotAWinningArbitrator,
    #[msg("This arbitrator has already claimed its share of this dispute's reward")]
    RewardAlreadyClaimed,
    #[msg("Arithmetic overflow")]
    Overflow,
}
