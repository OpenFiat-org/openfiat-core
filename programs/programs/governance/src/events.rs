//! On-chain event log for the governance lifecycle (OFS-4100 §9.4).
//!
//! Until these existed only the ban list emitted anything, so the
//! machinery that *produces* a ban — the proposal, the votes, the tally,
//! the deposit — was entirely unobservable. §9.4 treats that as a defect
//! rather than a missing convenience: governance whose history cannot be
//! reconstructed is governance nobody can check.
//!
//! # What "reconstructable from logs alone" requires
//!
//! Every event carries the identity an indexer joins on, not only the
//! delta: the `Proposal` address *and* its `id` (the address is what
//! other events reference, the id is what humans and the PDA seed use),
//! and for a vote, the voter and the role the weight came from. Nothing
//! here requires fetching an account to interpret — which matters most
//! for the accounts that are gone by the time anyone looks: a `Proposal`
//! records running totals rather than a vote history, and its deposit
//! state is a single boolean that says nothing about where the tokens
//! went.
//!
//! # Two events deliberately record an authorization, not an effect
//!
//! [`ConfigParameterChangeAuthorized`] and [`TreasurySpendAuthorized`]
//! are emitted by instructions that set `Proposal::executed` and perform
//! nothing else — no parameter is written, no funds move. Their names
//! and field names say "authorized", never "applied" or "transferred",
//! because an event that read as a completed disbursement would have an
//! indexer report protocol funds leaving an account they never left.

use anchor_lang::prelude::*;
use openfiat_programs_shared::{ProposalCategory, Role};

use crate::state::{BanReason, GovernanceAction, ProposalState};

/// OFS-7100 §12.2 requires listing *and* delisting to emit events: "An
/// exclusion nobody can audit or reverse is a worse failure than the
/// abuse it prevents." These two events are the audit half of that
/// sentence; `delist_wallet` is the reversible half.
///
/// Both name the authorizing proposal rather than a signer. There is no
/// signer worth naming any more: the transaction may be submitted by
/// anyone, and the decision belonged to the vote.
#[event]
pub struct WalletListed {
    pub wallet: Pubkey,
    pub reason: BanReason,
    pub evidence_hash: [u8; 32],
    /// The `Proposal` whose accepted vote authorized this listing.
    pub authorizing_proposal: Pubkey,
    pub proposal_id: u64,
    pub timestamp: i64,
}

#[event]
pub struct WalletDelisted {
    pub wallet: Pubkey,
    /// The `Proposal` whose accepted vote authorized the readmission —
    /// necessarily a different proposal from the one that listed the
    /// wallet, reached through exactly the same machinery.
    pub authorizing_proposal: Pubkey,
    pub proposal_id: u64,
    /// When the wallet was originally listed, carried over from the
    /// record being closed. Without it the delisting event alone cannot
    /// tell an auditor how long access was withheld — and the record
    /// that knew is gone by the time this is read.
    pub listed_at: i64,
    /// The proposal that listed the wallet, likewise carried over before
    /// the record holding it is closed. The pair is what lets an auditor
    /// set the exclusion decision beside the readmission one.
    pub listed_by_proposal: Pubkey,
    pub timestamp: i64,
}

/// The singleton config was created. Emitted by
/// `initialize_governance_config`.
///
/// Carries every parameter rather than "config initialized", because
/// these values are what every later proposal's `quorum_snapshot` and
/// `threshold_snapshot` are computed from — an indexer that cannot see
/// them cannot check that any proposal's snapshot was taken correctly.
#[event]
pub struct GovernanceConfigInitialized {
    pub governance_config: Pubkey,
    pub admin: Pubkey,
    pub mint: Pubkey,
    pub deposit_vault: Pubkey,
    pub total_open_supply: u64,
    pub quorum_bps: u16,
    pub threshold_simple_bps: u16,
    pub threshold_treasury_bps: u16,
    pub threshold_upgrade_bps: u16,
    pub quorum_upgrade_bps: u16,
    pub deposit_amount: u64,
    pub forfeit_destination: Pubkey,
    pub vote_lock_secs: i64,
    pub timestamp: i64,
}

/// The singleton config's parameters or forfeit destination changed.
/// Emitted by `update_governance_config`.
///
/// Mirrors [`GovernanceConfigInitialized`] field for field, deliberately:
/// an indexer replaying both in order holds the config's full state at
/// every point in time without ever fetching the account, which is the
/// only way to check that a proposal created at some past moment
/// snapshotted the values that were actually in force then.
///
/// Worth an event in its own right because `admin` alone can do this:
/// `vote_lock_secs` decides how long an accepted proposal is held before
/// it may act, and `forfeit_destination` decides where a missed-quorum
/// deposit goes. A silent change to either is exactly the sort of thing
/// participants need to be able to notice after the fact.
#[event]
pub struct GovernanceConfigUpdated {
    pub governance_config: Pubkey,
    pub admin: Pubkey,
    pub total_open_supply: u64,
    pub quorum_bps: u16,
    pub threshold_simple_bps: u16,
    pub threshold_treasury_bps: u16,
    pub threshold_upgrade_bps: u16,
    pub quorum_upgrade_bps: u16,
    pub deposit_amount: u64,
    pub forfeit_destination: Pubkey,
    pub vote_lock_secs: i64,
    pub timestamp: i64,
}

/// A proposal opened for voting. Emitted by `create_proposal`.
///
/// `action` is included in full, not as the `ProposalAction` account's
/// address. A ban proposal names the wallet it would exclude, and
/// OFS-7100 §12.2 requires a *proposed* exclusion to be as readable as an
/// enacted one — a reader who has to fetch a second account to learn what
/// they are being asked to vote on has been told nothing useful by the
/// log.
///
/// The two snapshots travel with it for the same reason: they are fixed
/// at this instant and never change, so the bar this proposal must clear
/// is knowable from its creation event alone rather than by guessing
/// which config values were live at the time.
#[event]
pub struct ProposalCreated {
    pub proposal: Pubkey,
    pub proposal_id: u64,
    pub proposer: Pubkey,
    pub category: ProposalCategory,
    pub title_hash: [u8; 32],
    pub summary_hash: [u8; 32],
    /// The action this proposal is entitled to perform if it passes,
    /// fixed at creation and immutable thereafter.
    pub action: GovernanceAction,
    pub proposal_action: Pubkey,
    /// Deposit taken from the proposer into the deposit vault, refunded
    /// or forfeited later by `refund_or_forfeit_deposit`.
    pub stake_deposit: u64,
    /// Effective-stake units of participation this proposal needs.
    pub quorum_snapshot: u64,
    /// Approval bar in basis points of votes cast.
    pub threshold_snapshot: u16,
    /// When voting closes. The opening time is this event's own
    /// `timestamp` — `Proposal::created_at` is written from the same
    /// clock read, so carrying it separately would be the same number
    /// twice.
    pub voting_ends_at: i64,
    pub timestamp: i64,
}

/// One vote counted toward a proposal. Emitted by `cast_vote`.
///
/// # `weight` is the weight the program counted
///
/// It is read from `staking::StakeAccount::effective_stake` — the
/// voter's on-chain stake under `role`, reduced to zero if it has fallen
/// below that role's minimum — and is the identical value added to
/// `votes_for`/`votes_against` in the same instruction. It is **never** a
/// figure the voter supplied.
///
/// That distinction is the security property of the entire voting path,
/// not a nicety. Off-chain, `crates/rpc`'s async vote verification
/// exists precisely because a gossiped vote's self-reported weight
/// cannot be trusted, and it overrides that number with the stake it
/// decodes from the chain. An event echoing a self-reported figure would
/// hand every indexer and explorer a tally that the chain does not
/// agree with, and would quietly undo that work — so `cast_vote` takes
/// no weight argument at all, and there is nothing self-reported here to
/// emit.
///
/// `voter_stake` and `role` are included so the claim is checkable: a
/// reader can fetch that exact account, apply the same minimum, and
/// arrive at the same number.
#[event]
pub struct VoteCast {
    pub proposal: Pubkey,
    pub proposal_id: u64,
    pub voter: Pubkey,
    /// The `StakeAccount` the weight was read from.
    pub voter_stake: Pubkey,
    /// Which role's stake was used. A wallet may hold stake under
    /// several roles and picks one per proposal; the `VoteRecord` PDA
    /// (keyed by proposal and voter only) is what limits it to one vote.
    pub role: Role,
    pub in_favor: bool,
    /// Effective stake counted for this vote — see this event's own doc.
    pub weight: u64,
    /// The proposal's running totals *after* this vote, so a replay can
    /// be checked against the account at any point without re-summing
    /// every prior event.
    pub votes_for: u64,
    pub votes_against: u64,
    /// Per `GovernanceConfig.vote_lock_secs`, recorded on the
    /// `VoteRecord`. Not yet enforced anywhere — see that field's own
    /// doc for the crate-dependency reason.
    pub locked_until: i64,
    pub timestamp: i64,
}

/// Voting closed and the result was computed. Emitted by
/// `tally_and_finalize`.
///
/// # Why the weights and `quorum_met`, not just the outcome
///
/// `quorum_met` is a separate fact from acceptance and decides something
/// acceptance does not: `refund_or_forfeit_deposit` keys on it alone, so
/// a proposal can be *rejected on the merits and still have its deposit
/// refunded*, and can be accepted only if quorum was met. An observer who
/// sees only "Rejected" cannot tell whether the subsequent deposit
/// settlement was correct — which makes the forfeiture unverifiable,
/// and an unverifiable forfeiture is indistinguishable from a
/// confiscation.
///
/// The tallied weights and the two snapshots travel with it so the whole
/// decision can be recomputed from this one event: quorum is
/// `votes_for + votes_against >= quorum_snapshot`, and acceptance is
/// `votes_for * 10_000 / total >= threshold_snapshot`. Reporting only
/// the verdict would require trusting it.
#[event]
pub struct ProposalFinalized {
    pub proposal: Pubkey,
    pub proposal_id: u64,
    pub category: ProposalCategory,
    pub votes_for: u64,
    pub votes_against: u64,
    /// `votes_for + votes_against` — the quorum measure, carried
    /// explicitly so an indexer never has to reproduce the addition that
    /// the program itself overflow-checked.
    pub total_cast: u64,
    /// The bar this proposal had to clear, snapshotted at creation.
    pub quorum_snapshot: u64,
    pub threshold_snapshot: u16,
    /// Whether participation reached `quorum_snapshot`. This, and not
    /// `state`, is what decides refund versus forfeiture.
    pub quorum_met: bool,
    /// `Accepted` or `Rejected`. A quorum miss and a genuine defeat both
    /// land on `Rejected`; `quorum_met` above is what tells them apart.
    pub state: ProposalState,
    pub timestamp: i64,
}

/// A proposal's deposit was returned to its proposer or forfeited.
/// Emitted by `refund_or_forfeit_deposit`.
///
/// `refunded` and `quorum_met` are both present although one determines
/// the other. That redundancy is the point: it lets an observer check the
/// branch was taken correctly against the same event, rather than having
/// to join back to the tally and trust that the `Proposal` was not
/// touched in between.
#[event]
pub struct ProposalDepositSettled {
    pub proposal: Pubkey,
    pub proposal_id: u64,
    pub proposer: Pubkey,
    pub amount: u64,
    pub mint: Pubkey,
    /// True when the deposit went back to the proposer, which happens
    /// exactly when quorum was met — independent of whether the proposal
    /// passed (OFS-4100 §5).
    pub refunded: bool,
    /// The token account the deposit actually landed in: the proposer's
    /// own when refunded, `GovernanceConfig.forfeit_destination` when
    /// forfeited.
    pub destination: Pubkey,
    /// Carried from the proposal so the branch above is checkable
    /// without a second lookup.
    pub quorum_met: bool,
    /// The proposal's final state, so a reader can see the refund was
    /// independent of it.
    pub state: ProposalState,
    pub timestamp: i64,
}

/// An accepted `Parameter` proposal was marked executed. Emitted by
/// `update_config_parameter`.
///
/// **No parameter was written.** That instruction records the
/// authorization and performs nothing, because the programs it would
/// need to mutate have no governance-aware authority yet (see its own
/// doc comment). The fields below therefore describe what the vote
/// *authorized*, and are named `authorized_*` so that an indexer
/// rendering this event cannot make it read as a change that took
/// effect. When the instruction gains a real effect, this event gains
/// the fields that prove it rather than being reinterpreted.
#[event]
pub struct ConfigParameterChangeAuthorized {
    pub proposal: Pubkey,
    pub proposal_id: u64,
    /// The program the authorized change is aimed at.
    pub target_program: Pubkey,
    /// The parameter named by the proposal, as free text — this program
    /// does not interpret it.
    pub parameter_key: String,
    pub authorized_value: u64,
    pub timestamp: i64,
}

/// An accepted `Treasury` proposal was marked executed. Emitted by
/// `authorize_treasury_spend`.
///
/// **No funds moved.** Governance holds no treasury to disburse from —
/// per OFS-4200 §1 it has "no asset custody beyond small proposal-stake
/// deposits", and the fee treasuries are token accounts owned by
/// external wallets, not a vault this program can sign for. The
/// instruction records the authorization only, so the amount and
/// destination below are named `authorized_*`: an event that read as a
/// completed transfer would have every explorer report protocol funds
/// leaving an account they are still sitting in.
#[event]
pub struct TreasurySpendAuthorized {
    pub proposal: Pubkey,
    pub proposal_id: u64,
    /// Where the vote authorized a future disbursement to go.
    pub authorized_destination: Pubkey,
    pub authorized_amount: u64,
    pub timestamp: i64,
}

/// AllenHark's first-year exception opened, and — the part that matters —
/// the exact moment it closes (OFS-4100 §5.1).
///
/// Emitted by both creation paths. Carries `expires_at` rather than only
/// `initialized_at` plus an implied duration, because the deadline is the
/// fact everyone downstream needs and deriving it from a constant means
/// an indexer trusting its own copy of that constant. There is no
/// matching `EmergencyAuthorityUpdated`, and there is no instruction that
/// could emit one: the deadline this announces is the deadline forever.
#[event]
pub struct EmergencyAuthorityInitialized {
    pub emergency_authority: Pubkey,
    /// Both holders, because §5.1 requires each to be presented as a
    /// first-class authority rather than one with the other as a
    /// footnote. Either key alone suffices; this is not a 2-of-2.
    pub primary_holder: Pubkey,
    pub secondary_holder: Pubkey,
    pub initialized_at: i64,
    pub expires_at: i64,
}

/// An on-chain proposal declared which off-chain proposal it is the
/// chain-side half of.
///
/// Emitted so an indexer can build the off-chain-to-on-chain join without
/// scanning every `Proposal` account for a field that is usually zero.
/// It records a *claim*, not a confirmed link — the off-chain half has to
/// name this proposal back before the two are joined, and only a reader
/// holding both records can tell. The field name says `offchain_id_hash`
/// rather than `offchain_proposal` for that reason.
#[event]
pub struct OffchainProposalLinked {
    pub proposal: Pubkey,
    pub proposal_id: u64,
    pub offchain_id_hash: [u8; 32],
    pub timestamp: i64,
}
