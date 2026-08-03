use anchor_lang::prelude::*;

#[error_code]
pub enum ErrorCode {
    #[msg("Only the fee config admin may perform this action")]
    Unauthorized,
    #[msg("Fee treasury split basis points must sum to exactly 10_000")]
    InvalidFeeSplit,
    #[msg("This arbitrator committed in an earlier round of this case and never revealed")]
    ArbitratorBarredFromCase,
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
    // Appended, never inserted: Anchor numbers error codes by
    // declaration order, so adding a variant above an existing one
    // renumbers every code after it and breaks clients matching on the
    // old number.
    #[msg("This wallet is on the governance ban list (OFS-7100 §12)")]
    WalletBanned,
    #[msg("This arbitrator's stake has not been held long enough to arbitrate")]
    ArbitratorStakeTooYoung,
    /// Deliberately says nothing about the threshold in force or how close
    /// the draw was. OFS-2400 keeps the per-case arbitrator threshold
    /// undisclosed, and a message that leaked "your ticket was 140, the
    /// threshold is 100" would let an attacker measure exactly how many
    /// more wallets they need.
    #[msg("This arbitrator was not drawn for this dispute case")]
    NotDrawnForThisCase,
    #[msg("The slot-hashes sysvar holds no usable entry to seed this case from")]
    SlotHashesUnavailable,
    #[msg("This fee config is not in the pre-migration layout")]
    FeeConfigAlreadyMigrated,
    #[msg("min_arbitrator_stake_age_secs may not be negative")]
    InvalidStakeAge,
    #[msg("arbitrator_sortition_bps must be below 10_000; use 0 to disable the draw")]
    InvalidSortitionThreshold,
    #[msg("This mint is not on the settlement-mint allowlist; governance must add it via update_fee_config")]
    SettlementMintNotAllowed,
    #[msg("The settlement-mint allowlist is full")]
    SettlementMintListFull,
    #[msg("The settlement-mint allowlist may not contain duplicates or the default pubkey")]
    InvalidSettlementMint,
    #[msg("The settlement-mint allowlist may not be empty")]
    EmptySettlementMintList,
    #[msg("The arbitration pool is not initialized; run initialize_arbitration_pool first")]
    ArbitrationPoolNotInitialized,
    #[msg("openfiat-staking has recovered nothing this claim has not already credited")]
    NothingToAbsorb,
    #[msg("The recovered tokens are not present in this vault's token account")]
    RecoveredTokensMissing,
    #[msg("This dispute case's deposit is already whole")]
    NoDepositShortfall,
    #[msg("This vault has no available balance to put toward the deposit shortfall")]
    NoLiquidityForShortfall,
    /// Raised only when a caller supplies *something* in the arbitration
    /// policy's remaining-account slot. Supplying nothing is legal and simply
    /// leaves the pool floor unenforced — see `published_pool_size`.
    #[msg("The supplied arbitration policy account is not the canonical singleton")]
    InvalidArbitrationPolicy,
}
