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
    #[msg("Arithmetic overflow")]
    Overflow,
}
