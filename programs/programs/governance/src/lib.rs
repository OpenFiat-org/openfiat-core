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
///
/// # What a passed proposal can actually do
///
/// A proposal carries a [`GovernanceAction`], fixed at creation and
/// stored in its own immutable [`ProposalAction`] account. `list_wallet`
/// and `delist_wallet` execute one: they take no privileged signer at
/// all, and instead require an `Accepted`, quorum-met, un-executed
/// proposal past its `vote_lock_secs` timelock whose action names the
/// exact wallet being listed or delisted. Any caller may submit them.
/// [`shared_logic::require_executable`] is the single definition of that
/// test.
///
/// `update_config_parameter` and `authorize_treasury_spend` are the
/// exception and remain record-only: they set `Proposal::executed` and
/// perform nothing, because the programs they would need to mutate have
/// no governance-aware authority yet. They carry
/// `GovernanceAction::None`. When they gain real effects they must adopt
/// `require_executable` — the whole point of it living in one place.
///
/// # AllenHark's first year, and its sunset
///
/// OFS-4100 §5.1 grants AllenHark a time-limited exception over
/// governance for one year after initialization, and insists it be
/// "enforced on-chain against a timestamp fixed at initialization and
/// immutable afterwards". [`EmergencyAuthority`] is that timestamp.
///
/// The exception's concrete content is the delay power the specification
/// itself identifies: the ability to write `GovernanceConfig.vote_lock_secs`,
/// which decides how long an accepted proposal waits before it may act.
/// It has two bounds and they are deliberately different in kind —
/// [`MAX_VOTE_LOCK_SECS`] caps how far the delay may be pushed (30 days),
/// and [`FIRST_YEAR_SECS`] caps how long anyone may push it at all. Past
/// the deadline, `update_governance_config` refuses to change the field.
///
/// **Nothing can move that deadline.** No instruction here takes
/// `EmergencyAuthority` mutably, neither creation path accepts a duration
/// or a deadline, and the window is not reachable through
/// `GovernanceAction`, so a passed governance vote cannot postpone it
/// either. Extending it takes a program upgrade — a separately
/// authorized, publicly visible act, not a governance action.
///
/// # Linking to the off-chain governance layer
///
/// `openfiat-core`'s `crates/governance` carries the same proposals as
/// gossiped, signed off-chain records. [`Proposal::offchain_id_hash`],
/// written by `link_offchain_proposal`, is this side's half of the join
/// between them; the off-chain `ProposalCreate` event names this
/// proposal's `u64` id as the other half. Neither half alone is a link —
/// a reader holding both is what establishes one, and a reader holding
/// only one is told so rather than shown a guess.
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

    /// Starts AllenHark's first-year exception on a deployment whose
    /// `GovernanceConfig` predates it. Permissionless and parameterless
    /// — see the instruction's own doc for why both are safe, and why
    /// there is no counterpart that could ever move the deadline.
    pub fn initialize_emergency_authority(
        ctx: Context<InitializeEmergencyAuthority>,
    ) -> Result<()> {
        crate::instructions::initialize_emergency_authority::handle_initialize_emergency_authority(
            ctx,
        )
    }

    /// Declares which off-chain proposal this on-chain one corresponds
    /// to. Proposer-only, `Voting`-only, and write-once.
    pub fn link_offchain_proposal(
        ctx: Context<LinkOffchainProposal>,
        offchain_id_hash: [u8; 32],
    ) -> Result<()> {
        crate::instructions::link_offchain_proposal::handle_link_offchain_proposal(
            ctx,
            offchain_id_hash,
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

    /// `action` is what this proposal will be entitled to do if it
    /// passes, fixed now and immutable thereafter — see
    /// `CreateProposal`'s doc for why it cannot be attached later.
    pub fn create_proposal(
        ctx: Context<CreateProposal>,
        id: u64,
        category: openfiat_programs_shared::ProposalCategory,
        title_hash: [u8; 32],
        summary_hash: [u8; 32],
        voting_period_secs: i64,
        action: GovernanceAction,
    ) -> Result<()> {
        crate::instructions::create_proposal::handle_create_proposal(
            ctx,
            id,
            category,
            title_hash,
            summary_hash,
            voting_period_secs,
            action,
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

    /// Adds a wallet to the protocol-wide ban list (OFS-7100 §12) by
    /// executing a passed proposal that names it. Permissionless: the
    /// authority is the vote, not the submitter. `reason` and
    /// `evidence_hash` are read from the proposal, which is why they are
    /// not arguments — see `ListWallet`'s doc comment.
    pub fn list_wallet(ctx: Context<ListWallet>, wallet: Pubkey) -> Result<()> {
        crate::instructions::list_wallet::handle_list_wallet(ctx, wallet)
    }

    /// Removes a wallet from the ban list, restoring deposit access
    /// protocol-wide (OFS-7100 §12.2). Runs through the identical
    /// mechanism as `list_wallet`, deliberately.
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
