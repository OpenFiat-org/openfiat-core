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

/// AllenHark's first-year governance exception and, more importantly, the
/// moment it ends (OFS-4100 §5.1). Singleton, at
/// `[EMERGENCY_AUTHORITY_SEED]`.
///
/// # Why this is a separate account and not four fields on `GovernanceConfig`
///
/// `GovernanceConfig` is a live singleton — one already exists on devnet
/// at `CQJG481GTPh7x6Y3eL39753tFDwChQQVzZ8mNrXKZTSq`, and it is the only
/// account this program owns there. Growing an `#[account]` struct makes
/// every already-allocated account of that type too small to deserialize,
/// so folding the sunset into `GovernanceConfig` would brick the deployed
/// config and take every instruction that reads it down with it. The same
/// reasoning that gave [`ProposalAction`] its own account applies here,
/// with the difference that this time the affected account demonstrably
/// exists.
///
/// # Why it is non-extendable, and how you can check
///
/// `expires_at` is written exactly once, by `init`, as
/// `initialized_at + FIRST_YEAR_SECS`. **No instruction in this program
/// takes this account as `mut`.** There is no `update_emergency_authority`,
/// no field on `GovernanceConfig` that feeds into the deadline, and no
/// proposal action that reaches it — so there is no transaction anyone
/// can send, holder or admin or a passed governance vote, that moves it.
/// That is a structural property, not a policy one: you can verify it by
/// reading this program's IDL and observing that `emergency_authority`
/// appears as a writable account in exactly the two instructions that
/// create it, and `tests/governance.ts` does exactly that so a future
/// instruction cannot quietly acquire write access.
///
/// The deadline is also not a parameter. Neither creation instruction
/// accepts one, so nothing about the initializing transaction — not its
/// sender, not its arguments — can influence how long the window lasts.
/// The only lever anyone has is *when* the clock starts, and starting it
/// can only ever bring the deadline nearer, never push it out.
#[account]
#[derive(InitSpace)]
pub struct EmergencyAuthority {
    /// Copied from [`crate::constants::ALLENHARK_PRIMARY_HOLDER`], not
    /// taken from the caller. Stored anyway rather than left implicit in
    /// the program binary, because §5.1 requires both holders to be
    /// presented as first-class authorities "in every application,
    /// explorer and document" — and an explorer can read an account,
    /// while reading a constant means trusting whoever published the
    /// source.
    pub primary_holder: Pubkey,
    /// Copied from [`crate::constants::ALLENHARK_SECONDARY_HOLDER`].
    /// Equal in authority to `primary_holder`: §5.1 is explicit that
    /// **either key alone suffices**, which is what makes the exception
    /// survive the loss of one key and not survive the compromise of one.
    pub secondary_holder: Pubkey,
    /// When the window opened, kept beside `expires_at` so an auditor can
    /// confirm the span is exactly `FIRST_YEAR_SECS` without knowing what
    /// the clock read that day.
    pub initialized_at: i64,
    /// When the exception ends. Written once by `init` and never again by
    /// anything — see the type-level doc.
    pub expires_at: i64,
    pub bump: u8,
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
    /// SHA-256 of the off-chain `openfiat-governance` proposal id this
    /// on-chain proposal is the chain-side half of, or all zeroes for
    /// "none claimed". Written once by `link_offchain_proposal` and never
    /// again.
    ///
    /// # Why a join key had to exist at all
    ///
    /// Off-chain proposals are gossiped records keyed by an author-chosen
    /// string; on-chain proposals are accounts keyed by a `u64`. Nothing
    /// correlated the two, so an interface that showed "the" proposal
    /// could not tell whether the chain agreed with it — it was showing
    /// one of two records and implying the other. `title_hash` and
    /// `summary_hash` look like they could serve, but neither pins its
    /// hash function anywhere and both describe *content*, which two
    /// unrelated proposals may legitimately share.
    ///
    /// # Why hashing the id rather than storing it
    ///
    /// An off-chain id is a variable-length string, and a `String` field
    /// on an `#[account]` means either a length cap invented here or an
    /// account whose size depends on user input. A 32-byte digest is
    /// fixed, costs nothing to compare, and the preimage is public — it
    /// is the id, which travels in the clear in every gossiped proposal
    /// event — so this hides nothing an observer needs.
    ///
    /// # Why this alone is a claim and not a link
    ///
    /// A proposer writes this unilaterally, so on its own it says only
    /// "this on-chain proposal claims that off-chain one". The off-chain
    /// proposal makes the matching claim in the other direction, inside
    /// the signed `ProposalCreate` event its author fixed at creation.
    /// The link exists only when both claims agree; see
    /// `openfiat_governance::onchain` for the side that checks it.
    pub offchain_id_hash: [u8; 32],
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
