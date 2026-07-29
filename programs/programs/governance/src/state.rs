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
    /// Consumed by every execution instruction, atomically with the
    /// effect it performs. This is the only thing that makes a passed
    /// proposal a *single-use* authorization: `list_wallet` and
    /// `delist_wallet` both refuse a proposal that already set it, in
    /// the same instruction that sets it, so a ban cannot be replayed.
    ///
    /// `update_config_parameter`/`authorize_treasury_spend` still only
    /// set it and return — see their doc comments for why they perform
    /// nothing yet.
    pub executed: bool,
    pub bump: u8,
}

/// What a proposal, if it passes, is authorized to *do* (OFS-4200 §6,
/// OFS-7100 §12.2).
///
/// Governance's problem before this existed was not that the ban list
/// picked the wrong authority — it was that an accepted proposal could
/// not cause any state change at all. `update_config_parameter` and
/// `authorize_treasury_spend` set `Proposal::executed = true` and return.
/// A vote decided nothing, so every capability that needed a decision
/// fell back to `GovernanceConfig.admin`.
///
/// Recording the action as *typed, structured state* rather than as a
/// hash is what makes an execution instruction able to check it. A
/// commitment hash would bind the action just as tightly, but only to a
/// caller who already knows the preimage; a voter deciding how to vote,
/// or an indexer auditing what governance has authorized, would have to
/// obtain that preimage out of band. §12.2 requires the ban list to be
/// auditable on-chain, and a *proposed* exclusion is exactly as worth
/// reading as an enacted one.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace, Debug)]
pub enum GovernanceAction {
    /// No on-chain effect. Informational and Standards proposals whose
    /// outcome is carried out off-chain, and the categories whose
    /// execution instructions are still record-only.
    None,
    /// Add `wallet` to the protocol-wide ban list (OFS-7100 §12).
    ListWallet {
        wallet: Pubkey,
        reason: BanReason,
        evidence_hash: [u8; 32],
    },
    /// Remove `wallet` from the ban list, restoring its deposit access
    /// protocol-wide (OFS-7100 §12.2).
    DelistWallet { wallet: Pubkey },
}

/// The action a `Proposal` authorizes, at `[PROPOSAL_ACTION_SEED, proposal]`.
///
/// A separate account rather than a field on `Proposal`, for two
/// reasons. `Proposal`'s layout is already allocated on devnet, and
/// growing an `#[account]` struct makes every existing account of that
/// type fail to deserialize — which for `Proposal` would strand
/// `refund_or_forfeit_deposit` on any proposal created before the
/// upgrade. And an action is optional-shaped data on a required-shaped
/// account: keeping it separate leaves `Proposal` describing the vote
/// and this describing the consequence.
///
/// Created by `create_proposal` and never written again — `init` is the
/// whole immutability argument. It exists from the moment voting opens,
/// so no action can be attached to a proposal people have already voted
/// on, and none can be swapped after the vote succeeds.
#[account]
#[derive(InitSpace)]
pub struct ProposalAction {
    /// The proposal this authorizes. Duplicates the seed for the same
    /// reason `BanRecord::wallet` does: a PDA address does not reveal
    /// its seeds, so without this an indexer could enumerate every
    /// pending action without being able to say which vote decides it.
    pub proposal: Pubkey,
    pub action: GovernanceAction,
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

/// Grounds for a listing, taken from the examples OFS-7100 §12 gives.
///
/// Recorded on-chain because §12 requires the list to be auditable and
/// §15 requires false positives to be correctable: "banned, reason
/// unknown" is not something an erroneously-listed wallet can argue
/// against. `Other` exists so an unforeseen ground does not force a
/// misleading choice among the four named ones — the real detail lives
/// in the evidence the hash commits to.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace, Debug)]
pub enum BanReason {
    StolenFunds,
    Sanctions,
    Phishing,
    Scam,
    Other,
}

/// One banned wallet (OFS-7100 §12). Lives at `[BAN_SEED, wallet]`.
///
/// The account's *existence* is the ban — the enforcing programs never
/// deserialize this struct, they only observe that the address is
/// occupied (see `openfiat_programs_shared::wallet_is_banned`). The
/// fields exist for auditability rather than enforcement, which is why
/// adding one later is safe: no gate depends on this layout.
///
/// Storing `wallet` explicitly duplicates the seed. That is deliberate:
/// a PDA seed cannot be recovered from the address, so without this
/// field an indexer scanning by discriminator could list every ban
/// record on chain without being able to say *whom* any of them ban.
#[account]
#[derive(InitSpace)]
pub struct BanRecord {
    pub wallet: Pubkey,
    pub reason: BanReason,
    /// Hash of the off-chain evidence supporting the listing, following
    /// `Proposal`'s title/summary-hash pattern. §12.2 draws the line
    /// that a risk intelligence provider *publishes evidence* while
    /// governance *decides exclusion*; this field is where the published
    /// evidence is pinned so a listing can be contested against a fixed
    /// artefact rather than a changing one.
    pub evidence_hash: [u8; 32],
    pub listed_at: i64,
    /// The `Proposal` that authorized this listing — not the wallet that
    /// submitted the transaction, which is nobody in particular. This is
    /// the field that makes a listing contestable under §15: it leads
    /// from the ban back to the vote, the tally, and the voting record
    /// that produced it. The submitter is already recoverable from the
    /// transaction itself and confers no authority worth recording.
    pub authorizing_proposal: Pubkey,
    pub bump: u8,
}
