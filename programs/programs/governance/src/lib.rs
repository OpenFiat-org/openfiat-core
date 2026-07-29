pub mod constants;
pub mod error;
pub mod events;
pub mod instructions;
pub mod shared_logic;
pub mod state;

use anchor_lang::prelude::*;
use openfiat_programs_shared::Role;

pub use constants::*;
pub use events::*;
pub use instructions::*;
pub use state::*;

declare_id!("AVJfKUjHsizkGGUy8sdz4Xma2hVgmgvgg8GmUMs8E4eE");

/// `openfiat-governance` — proposals, voting, parameter updates, and
/// treasury spend authorization (OFS-4200 §6). Phase 5b. `cast_vote`
/// weighs each vote by reading the voter's `openfiat-staking`
/// `StakeAccount` directly (no CPI dispatch — see that crate's own doc
/// comment for why), the same pattern `openfiat-escrow`'s Phase 4b
/// dispute bridge already established.
#[program]
pub mod governance {
    use super::*;

    pub fn initialize_governance_config(
        ctx: Context<InitializeGovernanceConfig>,
        params: InitializeGovernanceConfigParams,
    ) -> Result<()> {
        crate::instructions::initialize_governance_config::handle_initialize_governance_config(
            ctx, params,
        )
    }

    /// Corrects the singleton config after initialization — see the
    /// instruction's own doc for why `forfeit_destination` moved from a
    /// param to an account.
    pub fn update_governance_config(
        ctx: Context<UpdateGovernanceConfig>,
        params: UpdateGovernanceConfigParams,
    ) -> Result<()> {
        crate::instructions::update_governance_config::handle_update_governance_config(ctx, params)
    }

    pub fn create_proposal(
        ctx: Context<CreateProposal>,
        id: u64,
        category: openfiat_programs_shared::ProposalCategory,
        title_hash: [u8; 32],
        summary_hash: [u8; 32],
        voting_period_secs: i64,
    ) -> Result<()> {
        crate::instructions::create_proposal::handle_create_proposal(
            ctx,
            id,
            category,
            title_hash,
            summary_hash,
            voting_period_secs,
        )
    }

    pub fn cast_vote(ctx: Context<CastVote>, in_favor: bool, role: Role) -> Result<()> {
        crate::instructions::cast_vote::handle_cast_vote(ctx, in_favor, role)
    }

    pub fn tally_and_finalize(ctx: Context<TallyAndFinalize>) -> Result<()> {
        crate::instructions::tally_and_finalize::handle_tally_and_finalize(ctx)
    }

    pub fn refund_or_forfeit_deposit(ctx: Context<RefundOrForfeitDeposit>) -> Result<()> {
        crate::instructions::refund_or_forfeit_deposit::handle_refund_or_forfeit_deposit(ctx)
    }

    pub fn update_config_parameter(
        ctx: Context<UpdateConfigParameter>,
        target_program: Pubkey,
        parameter_key: String,
        new_value: u64,
    ) -> Result<()> {
        crate::instructions::update_config_parameter::handle_update_config_parameter(
            ctx,
            target_program,
            parameter_key,
            new_value,
        )
    }

    /// Adds a wallet to the protocol-wide ban list (OFS-7100 §12). Read
    /// `ListWallet`'s doc comment before wiring this into any interface:
    /// the authority is a single admin key, not a governance vote.
    pub fn list_wallet(
        ctx: Context<ListWallet>,
        wallet: Pubkey,
        reason: BanReason,
        evidence_hash: [u8; 32],
    ) -> Result<()> {
        crate::instructions::list_wallet::handle_list_wallet(ctx, wallet, reason, evidence_hash)
    }

    /// Removes a wallet from the ban list, restoring deposit access
    /// protocol-wide (OFS-7100 §12.2).
    pub fn delist_wallet(ctx: Context<DelistWallet>, wallet: Pubkey) -> Result<()> {
        crate::instructions::delist_wallet::handle_delist_wallet(ctx, wallet)
    }

    pub fn authorize_treasury_spend(
        ctx: Context<AuthorizeTreasurySpend>,
        destination: Pubkey,
        amount: u64,
    ) -> Result<()> {
        crate::instructions::authorize_treasury_spend::handle_authorize_treasury_spend(
            ctx,
            destination,
            amount,
        )
    }
}

/// The ban gate lives on a constant in `openfiat-programs-shared` because
/// `staking` cannot depend on this crate without closing a dependency
/// cycle (see that crate's module doc). That makes the id below a second
/// copy of `declare_id!`'s, and a divergence would not fail any build —
/// it would silently point every gate in every program at a program that
/// never writes ban records, disabling the whole ban list at once. This
/// test is the only thing standing between that and a green CI run.
#[cfg(test)]
mod tests {
    #[test]
    fn governance_program_id_matches_declare_id() {
        assert_eq!(
            openfiat_programs_shared::GOVERNANCE_PROGRAM_ID,
            super::ID,
            "shared::GOVERNANCE_PROGRAM_ID must equal governance's declare_id!"
        );
    }
}
