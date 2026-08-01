use anchor_lang::prelude::*;

#[error_code]
pub enum ErrorCode {
    #[msg("Only the staking config admin may perform this action")]
    Unauthorized,
    #[msg("Only the configured slashing authority may perform this action")]
    NotSlashingAuthority,
    #[msg("Only the configured rewards authority may perform this action")]
    NotRewardsAuthority,
    #[msg("slash_bps must be between 0 and 10_000")]
    InvalidSlashBps,
    #[msg("unbonding_period_secs must be greater than zero")]
    InvalidUnbondingPeriod,
    #[msg("Requested amount exceeds this stake account's staked (non-unbonding) balance")]
    InsufficientStakedAmount,
    #[msg("This stake account's unbonding period has not yet elapsed")]
    StillUnbonding,
    #[msg("This stake account has no unbonding balance to withdraw")]
    NoUnbondingBalance,
    #[msg("This stake account has no pending rewards to claim")]
    NoPendingRewards,
    #[msg("Amount must be greater than zero")]
    ZeroAmount,
    #[msg("Resulting balance would be below this role's minimum stake without being a full exit")]
    StakeBelowRoleMinimum,
    #[msg("This staking config is not in the pre-migration layout")]
    AlreadyMigrated,
    #[msg("Account is denominated in a different mint than the staking config")]
    WrongMint,
    #[msg("An authority may not be the default (zero) pubkey")]
    ZeroAuthority,
    #[msg("Arithmetic overflow")]
    Overflow,
    // Appended, never inserted: Anchor numbers error codes by
    // declaration order, so adding a variant above an existing one
    // renumbers every code after it and breaks clients matching on the
    // old number.
    #[msg("This wallet is on the governance ban list (OFS-7100 §12)")]
    WalletBanned,
    #[msg("This stake account is not in the pre-migration layout")]
    StakeAccountAlreadyMigrated,
    #[msg("Account is not the canonical stake account for the owner and role it claims")]
    NotAStakeAccount,
    #[msg("Account is not an openfiat-escrow stake recovery claim for this merchant and mint")]
    NotARecoveryClaim,
    #[msg("Account is not the canonical stake recovery receipt for this merchant")]
    NotARecoveryReceipt,
    #[msg("This merchant owes nothing that has not already been recovered")]
    NothingToRecover,
    #[msg("This stake account holds no balance left to recover from")]
    NoStakeToRecoverFrom,
    #[msg(
        "This merchant's arbitration deposit debt must be recovered before stake may be withdrawn"
    )]
    StakeRecoveryOutstanding,
}
