use anchor_lang::prelude::*;
use openfiat_programs_shared::ProposalCategory;

/// Singleton governance configuration (OFS-4200 §6), governance-updatable
/// in a later phase (once this program can amend itself via a Parameter
/// proposal) — for now, updatable only by `admin`.
///
/// `total_open_supply` isn't in OFS-4200's own field list — quorum needs
/// *some* denominator to compute a percentage against, and this
/// workspace's OPEN mint has a fixed, known total supply (OFS-4100 §1),
/// which is the standard quorum denominator for token-weighted
/// governance (percentage of total supply that participated, not
/// percentage of votes cast — that's the separate approval threshold).
#[account]
#[derive(InitSpace)]
pub struct GovernanceConfig {
    pub admin: Pubkey,
    pub mint: Pubkey,
    pub total_open_supply: u64,
    pub quorum_bps: u16,
    pub threshold_simple_bps: u16,
    pub threshold_treasury_bps: u16,
    pub threshold_upgrade_bps: u16,
    pub quorum_upgrade_bps: u16,
    pub deposit_amount: u64,
    /// Where a proposal's forfeited deposit goes (quorum missed) — not
    /// named in OFS-4200 §6, mirrors `escrow`/`staking`'s own
    /// forfeit/slash-destination pattern.
    pub forfeit_destination: Pubkey,
    pub vote_lock_secs: i64,
    pub bump: u8,
    pub deposit_vault_bump: u8,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace, Debug)]
pub enum ProposalState {
    Draft,
    Voting,
    Accepted,
    Rejected,
}

/// OFS-4200 §6. `create_proposal` goes straight to `Voting` — nothing in
/// OFS-4200 defines a separate "publish" transition out of `Draft`, so
/// `Draft` is kept as a defined-but-currently-unreachable state for a
/// future amendment rather than invented machinery with no spec basis.
#[account]
#[derive(InitSpace)]
pub struct Proposal {
    pub id: u64,
    pub category: ProposalCategory,
    pub proposer: Pubkey,
    /// Full title/summary text lives off-chain (this workspace's own
    /// gossip/session-sync machinery, per OFS-4200 §6) — only content
    /// hashes are recorded on-chain.
    pub title_hash: [u8; 32],
    pub summary_hash: [u8; 32],
    pub stake_deposit: u64,
    pub votes_for: u64,
    pub votes_against: u64,
    /// This category's quorum requirement, in effective-stake units,
    /// snapshotted from `GovernanceConfig` at creation time so a later
    /// config change never retroactively changes an in-flight
    /// proposal's requirement.
    pub quorum_snapshot: u64,
    /// This category's approval threshold (basis points of votes cast),
    /// snapshotted alongside `quorum_snapshot` for the same reason.
    pub threshold_snapshot: u16,
    pub created_at: i64,
    pub voting_ends_at: i64,
    pub state: ProposalState,
    /// Set by `tally_and_finalize` — distinct from `state == Accepted`,
    /// since a proposal can meet quorum and still be `Rejected` on the
    /// merits; deposit refund policy depends on quorum alone (OFS-4100
    /// §5: "refunded if quorum is met... independent of whether the
    /// proposal itself passes").
    pub quorum_met: bool,
    pub deposit_settled: bool,
    /// Set by `update_config_parameter`/`authorize_treasury_spend` —
    /// see those instructions' own doc comments for why this records
    /// the authorization rather than performing a live cross-program
    /// mutation.
    pub executed: bool,
    pub bump: u8,
}

/// OFS-4200 §6 — its own existence at `[VOTE_RECORD_SEED, proposal, voter]`
/// is the double-vote guard.
#[account]
#[derive(InitSpace)]
pub struct VoteRecord {
    pub proposal: Pubkey,
    pub voter: Pubkey,
    pub weight: u64,
    pub in_favor: bool,
    /// Recorded per `GovernanceConfig.vote_lock_secs`. Not enforced
    /// anywhere yet — doing so would require `staking` to depend on
    /// `governance` too, a circular crate dependency this workspace
    /// avoids; enforcement is a documented future wiring gap, not a
    /// silent omission.
    pub locked_until: i64,
    pub bump: u8,
}
