use anchor_lang::prelude::*;

#[error_code]
pub enum ErrorCode {
    #[msg("Only the governance config admin may perform this action")]
    Unauthorized,
    #[msg("quorum_bps, threshold_*_bps, and quorum_upgrade_bps must each be between 0 and 10_000")]
    InvalidBps,
    #[msg("vote_lock_secs must be greater than zero")]
    InvalidVoteLock,
    #[msg("Voting has not yet ended for this proposal")]
    VotingStillOpen,
    #[msg("This proposal is not in the Voting state")]
    NotInVotingState,
    #[msg("This proposal has already been tallied")]
    AlreadyTallied,
    #[msg("This proposal's deposit has already been settled (refunded or forfeited)")]
    DepositAlreadySettled,
    #[msg("This proposal must be tallied before its deposit can be settled")]
    NotYetTallied,
    #[msg("This action requires the proposal to be in the Accepted state")]
    ProposalNotAccepted,
    #[msg("This action requires the Parameter category")]
    WrongCategoryForParameterUpdate,
    #[msg("This action requires the Treasury category")]
    WrongCategoryForTreasurySpend,
    #[msg("This proposal's execution has already been recorded")]
    AlreadyExecuted,
    #[msg("Arithmetic overflow")]
    Overflow,
    // Appended rather than inserted: Anchor derives error codes from
    // declaration order, so adding a variant above an existing one
    // silently renumbers every code after it and invalidates any client
    // matching on the old number.
    #[msg("The supplied mint does not match the one recorded on GovernanceConfig")]
    MintMismatch,
    #[msg("This wallet is on the governance ban list (OFS-7100 §12)")]
    WalletBanned,
    #[msg("This proposal did not meet quorum, so it authorizes nothing")]
    QuorumNotMet,
    #[msg(
        "This proposal's execution timelock (vote_lock_secs after voting closed) has not elapsed"
    )]
    ExecutionTimelockActive,
    #[msg("Ban-list actions require the Standards category")]
    WrongCategoryForBanAction,
    #[msg("This proposal does not authorize this action against this wallet")]
    ProposalActionMismatch,
    #[msg("vote_lock_secs exceeds MAX_VOTE_LOCK_SECS")]
    VoteLockTooLong,
    #[msg(
        "AllenHark's first-year governance exception has expired; vote_lock_secs is now frozen (OFS-4100 §5.1)"
    )]
    EmergencyPowersExpired,
    #[msg("This proposal is already linked to an off-chain proposal, and the link is immutable")]
    OffchainLinkAlreadySet,
    #[msg("Only the proposal's own proposer may link it to an off-chain proposal")]
    NotTheProposer,
    #[msg("An off-chain link must be a real digest, not all zeroes")]
    EmptyOffchainIdHash,
}
