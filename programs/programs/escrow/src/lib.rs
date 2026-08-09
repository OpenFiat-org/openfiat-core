pub mod arbitration;
pub mod constants;
pub mod error;
pub mod events;
pub mod instructions;
pub mod state;

use anchor_lang::prelude::*;

pub use arbitration::*;
pub use constants::*;
pub use events::*;
pub use instructions::*;
pub use state::*;

declare_id!("CYdn27x69hQ8WBxBeWRGpr9c8B4dcKj8GvyBn6Sdma9s");

/// `openfiat-escrow` — Liquidity Vault + Trade Escrow Vault custody
/// (OFS-4200 §4). Phase 4: full vault lifecycle — create, deposit,
/// reserve, withdraw, create/fund a trade escrow, approve, release,
/// cancel, expire. Phase 4b: the dispute-to-chain bridge —
/// `open_dispute_case` freezes the escrow and opens a `DisputeCase`;
/// arbitrators `commit_dispute_vote`/`reveal_dispute_vote`;
/// `execute_dispute_outcome` permissionlessly tallies the case's own
/// on-chain, stake-weighted votes (reading `openfiat-staking`'s
/// `StakeAccount` directly) rather than trusting a caller-supplied
/// outcome.
#[program]
pub mod escrow {
    use super::*;

    pub fn initialize_fee_config(
        ctx: Context<InitializeFeeConfig>,
        params: InitializeFeeConfigParams,
    ) -> Result<()> {
        crate::instructions::initialize_fee_config::handle_initialize_fee_config(ctx, params)
    }

    /// Creates the singleton arbitration pool. One-time, admin-gated —
    /// see `initialize_arbitration_pool`.
    pub fn initialize_arbitration_pool(ctx: Context<InitializeArbitrationPool>) -> Result<()> {
        crate::instructions::initialize_arbitration_pool::handle_initialize_arbitration_pool(ctx)
    }

    /// Publishes how many wallets are eligible to arbitrate, so a case can
    /// refuse to open a round the pool cannot staff (OFS-4100 Annex A).
    /// Admin-gated, creates the singleton on first use, and zero means
    /// "unpublished" — which leaves the pool floor off. See
    /// `publish_arbitrator_pool_size`.
    pub fn publish_arbitrator_pool_size(
        ctx: Context<PublishArbitratorPoolSize>,
        eligible_arbitrators: u32,
    ) -> Result<()> {
        crate::instructions::publish_arbitrator_pool_size::handle_publish_arbitrator_pool_size(
            ctx,
            eligible_arbitrators,
        )
    }

    /// Charges a merchant the advertisement-listing fee against their OPEN
    /// liquidity vault. `advertisement_id` is the off-chain listing's own
    /// id, recorded only in the emitted event.
    pub fn charge_ad_listing_fee(
        ctx: Context<ChargeAdListingFee>,
        advertisement_id: [u8; 32],
    ) -> Result<()> {
        crate::instructions::charge_ad_listing_fee::handle_charge_ad_listing_fee(
            ctx,
            advertisement_id,
        )
    }

    /// Pays one arbitrator their pro-rata share of a forfeited arbitration
    /// deposit. Pull-based — see `claim_arbitration_reward`.
    pub fn claim_arbitration_reward(ctx: Context<ClaimArbitrationReward>) -> Result<()> {
        crate::instructions::claim_arbitration_reward::handle_claim_arbitration_reward(ctx)
    }

    /// One-shot layout migration adding the two arbitrator-eligibility
    /// parameters to an already-deployed `FeeConfig`. Both come out
    /// disabled — see `migrate_fee_config`'s own doc for why that is the
    /// only correct value and not a placeholder.
    pub fn migrate_fee_config(ctx: Context<MigrateFeeConfig>) -> Result<()> {
        crate::instructions::migrate_fee_config::handle_migrate_fee_config(ctx)
    }

    pub fn update_fee_config(
        ctx: Context<UpdateFeeConfig>,
        params: UpdateFeeConfigParams,
    ) -> Result<()> {
        crate::instructions::update_fee_config::handle_update_fee_config(ctx, params)
    }

    pub fn create_liquidity_vault(ctx: Context<CreateLiquidityVault>) -> Result<()> {
        crate::instructions::create_liquidity_vault::handle_create_liquidity_vault(ctx)
    }

    pub fn deposit_liquidity(ctx: Context<DepositLiquidity>, amount: u64) -> Result<()> {
        crate::instructions::deposit_liquidity::handle_deposit_liquidity(ctx, amount)
    }

    pub fn reserve_liquidity(ctx: Context<ReserveLiquidity>, amount: u64) -> Result<()> {
        crate::instructions::reserve_liquidity::handle_reserve_liquidity(ctx, amount)
    }

    pub fn withdraw_liquidity(ctx: Context<WithdrawLiquidity>, amount: u64) -> Result<()> {
        crate::instructions::withdraw_liquidity::handle_withdraw_liquidity(ctx, amount)
    }

    pub fn create_trade_escrow(
        ctx: Context<CreateTradeEscrow>,
        reservation_id: u64,
        amount: u64,
        timeout_secs: i64,
    ) -> Result<()> {
        crate::instructions::create_trade_escrow::handle_create_trade_escrow(
            ctx,
            reservation_id,
            amount,
            timeout_secs,
        )
    }

    pub fn fund_trade_escrow(ctx: Context<FundTradeEscrow>) -> Result<()> {
        crate::instructions::fund_trade_escrow::handle_fund_trade_escrow(ctx)
    }

    pub fn approve_settlement(ctx: Context<ApproveSettlement>) -> Result<()> {
        crate::instructions::approve_settlement::handle_approve_settlement(ctx)
    }

    pub fn release_escrow(ctx: Context<ReleaseEscrow>) -> Result<()> {
        crate::instructions::release_escrow::handle_release_escrow(ctx)
    }

    pub fn cancel_reservation(ctx: Context<CancelReservation>) -> Result<()> {
        crate::instructions::cancel_reservation::handle_cancel_reservation(ctx)
    }

    pub fn expire_reservation(ctx: Context<ExpireReservation>) -> Result<()> {
        crate::instructions::expire_reservation::handle_expire_reservation(ctx)
    }

    pub fn open_dispute_case(
        ctx: Context<OpenDisputeCase>,
        commit_window_secs: i64,
        reveal_window_secs: i64,
    ) -> Result<()> {
        crate::instructions::open_dispute_case::handle_open_dispute_case(
            ctx,
            commit_window_secs,
            reveal_window_secs,
        )
    }

    pub fn commit_dispute_vote(
        ctx: Context<CommitDisputeVote>,
        commitment: [u8; 32],
    ) -> Result<()> {
        crate::instructions::commit_dispute_vote::handle_commit_dispute_vote(ctx, commitment)
    }

    pub fn reveal_dispute_vote(
        ctx: Context<RevealDisputeVote>,
        outcome: openfiat_programs_shared::DisputeOutcome,
        salt: [u8; 32],
    ) -> Result<()> {
        crate::instructions::reveal_dispute_vote::handle_reveal_dispute_vote(ctx, outcome, salt)
    }

    pub fn execute_dispute_outcome(ctx: Context<ExecuteDisputeOutcome>) -> Result<()> {
        crate::instructions::execute_dispute_outcome::handle_execute_dispute_outcome(ctx)
    }

    /// Credits a merchant's OPEN vault with stake `openfiat-staking` has
    /// already recovered against their arbitration-deposit debt
    /// (OFS-4100 §9.3). Permissionless and parameterless — see
    /// `absorb_stake_recovery`.
    pub fn absorb_stake_recovery(ctx: Context<AbsorbStakeRecovery>) -> Result<()> {
        crate::instructions::absorb_stake_recovery::handle_absorb_stake_recovery(ctx)
    }

    /// Moves vault liquidity into the arbitration pool to make an
    /// under-funded deposit good, while the case is still open.
    /// Permissionless — see `top_up_arbitration_deposit`.
    pub fn top_up_arbitration_deposit(ctx: Context<TopUpArbitrationDeposit>) -> Result<()> {
        crate::instructions::top_up_arbitration_deposit::handle_top_up_arbitration_deposit(ctx)
    }
}
