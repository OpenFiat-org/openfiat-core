use anchor_lang::prelude::*;

/// PDA seed for the singleton `GovernanceConfig` account (OFS-4200 §6).
#[constant]
pub const GOVERNANCE_CONFIG_SEED: &[u8] = b"governance_config";

/// PDA seed for the token vault holding proposal-stake deposits until
/// `refund_or_forfeit_deposit` runs. Not named explicitly in OFS-4200
/// §6 — this workspace's own extension, matching every other program's
/// "singleton config owns a singleton vault" pattern.
#[constant]
pub const DEPOSIT_VAULT_SEED: &[u8] = b"deposit_vault";

/// PDA seed for a `Proposal`: `[SEED, id.to_le_bytes()]` (OFS-4200 §6).
#[constant]
pub const PROPOSAL_SEED: &[u8] = b"proposal";

/// PDA seed for a `VoteRecord`: `[SEED, proposal, voter]` (OFS-4200 §6)
/// — its existence is itself the double-vote guard.
#[constant]
pub const VOTE_RECORD_SEED: &[u8] = b"vote";

/// PDA seed for a `ProposalAction`: `[SEED, proposal]`. Keyed by the
/// proposal's own address, so a proposal has exactly one action and an
/// action belongs to exactly one proposal — the binding that stops a
/// vote to ban wallet A from being redeemed against wallet B.
#[constant]
pub const PROPOSAL_ACTION_SEED: &[u8] = b"proposal_action";

/// Upper bound on `GovernanceConfig.vote_lock_secs` (30 days).
///
/// `vote_lock_secs` is the delay between a proposal being accepted and
/// its action becoming executable, and `admin` can still write it. Left
/// unbounded, `admin` could set it to `i64::MAX` and make every accepted
/// proposal permanently unexecutable — which would restore, by the back
/// door, exactly the power this program's ban list was re-gated to
/// remove: a single key able to block a delisting indefinitely. The
/// bound does not remove the delay power, it caps it at something the
/// protocol can wait out.
///
/// OFS-4100 §5.1 puts this ceiling *inside* the first-year exception
/// rather than beside it, because writing `vote_lock_secs` is the delay
/// power: "left unbounded it is an emergency power wearing a different
/// hat". This bound is the first of its two limits — how far the delay
/// may be pushed. [`FIRST_YEAR_SECS`] is the second — how long the power
/// to push it exists at all. `update_governance_config` enforces both.
#[constant]
pub const MAX_VOTE_LOCK_SECS: i64 = 30 * 24 * 60 * 60;

/// Basis-points denominator (10_000 = 100%), matching every other
/// program in this workspace.
#[constant]
pub const BPS_DENOMINATOR: u64 = 10_000;

/// PDA seed for the singleton `EmergencyAuthority` (OFS-4100 §5.1).
#[constant]
pub const EMERGENCY_AUTHORITY_SEED: &[u8] = b"emergency_authority";

/// How long AllenHark's governance exception lasts: one year from the
/// moment [`crate::state::EmergencyAuthority`] is created, and not one
/// second longer (OFS-4100 §5.1, "The first year after initialization,
/// and no longer").
///
/// A compiled-in duration rather than an initialization parameter, and
/// that is the entire point. `expires_at` is computed from this once, at
/// `init`, and no instruction in this program ever takes the account
/// mutably again — so there is no transaction, privileged or otherwise,
/// that can move the deadline. A sunset a governance vote can postpone is
/// not a sunset; a sunset whose holder can rewrite its own deadline is
/// not one either. Extending it requires a program upgrade, which is a
/// visible, separately-authorized act rather than a governance action.
///
/// 365 days flat, not 365.25: a deadline anyone can recompute from
/// `initialized_at` with integer arithmetic is worth more here than one
/// that tracks the leap cycle to within six hours.
#[constant]
pub const FIRST_YEAR_SECS: i64 = 365 * 24 * 60 * 60;

/// The first of AllenHark's two governance exception keys (OFS-4100
/// §5.1).
///
/// **Either key alone suffices — this is not a 2-of-2.** Both are
/// first-class authorities and must be presented as such wherever they
/// appear; §5.1 is explicit that neither is a footnote to the other.
/// Recorded on-chain by `initialize_emergency_authority` so an explorer
/// can read the holders off the account rather than trusting a document.
///
/// Compiled in rather than passed at initialization, because a holder
/// supplied by whoever happened to submit the initialization transaction
/// is a holder that transaction's sender chose. These are the values
/// OFS-4100 signed off, and `initialize_emergency_authority` therefore
/// needs no parameters at all — which is also what makes it safe to leave
/// permissionless.
#[constant]
pub const ALLENHARK_PRIMARY_HOLDER: Pubkey =
    pubkey!("ALLENLMtV1zEAHT3xpVryqcbdPCB8c9JhM1Jdbe5XHg5");

/// AllenHark's second governance exception key (OFS-4100 §5.1). See
/// [`ALLENHARK_PRIMARY_HOLDER`] — the two are equal in authority.
#[constant]
pub const ALLENHARK_SECONDARY_HOLDER: Pubkey =
    pubkey!("A11ENCKCBxZxEbXQmqs6mTmJkP8gjcA7xqfLD5BxfRpp");

/// PDA seed for a `BanRecord`: `[BAN_SEED, wallet]` (OFS-7100 §12).
///
/// An alias of the shared constant, not a second literal: the enforcing
/// programs derive the same address from
/// `openfiat_programs_shared::BAN_SEED`, and a typo here that silently
/// disagreed with them would leave every gate deriving a permanently
/// empty address. Restated as a `#[constant]` purely so the seed appears
/// in this program's IDL for SDK consumers.
#[constant]
pub const BAN_SEED: &[u8] = openfiat_programs_shared::BAN_SEED;

/// Minimum on-chain voting window for a governance proposal (OFS-4000).
///
/// [DEVNET VALUE] 30 seconds, so governance-cycle tests wait seconds, not
/// days. MUST be raised to 604_800 (7 days) before mainnet — this is a
/// compile-time constant, so the mainnet program build has to bump it.
/// Tracked as a hard pre-mainnet gate (mainnet launch register).
#[constant]
pub const MIN_VOTING_PERIOD_SECS: i64 = 30;
