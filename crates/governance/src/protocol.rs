//! Wire-level constants. Event names follow OFS-8100 (OETR)'s Governance
//! Events vocabulary — OFS-4000 itself only describes governance actions
//! narratively (§23), it doesn't give an exact PascalCase event list the
//! way OFS-2100/OFS-2400 do. `ProposalPassed`/`ProposalRejected` (also in
//! OETR) aren't emitted as separate signed events: like disputes'
//! consensus, resolution is a pure local derivation from already-verified
//! vote events plus the voting deadline, computed identically by every
//! node — see `store::resolve_expired`.

use std::time::Duration;

pub const OFS_SPEC: u16 = 4000;

pub const EVENT_CREATED: &str = "ProposalCreated";
pub const EVENT_VOTE_CAST: &str = "VoteCast";
pub const EVENT_WITHDRAWN: &str = "ProposalCancelled";
pub const EVENT_ACTIVATED: &str = "ProposalActivated";

/// §13: "Voting periods are governance configurable" — this fixed
/// default is `[PROPOSED — NEEDS SIGN-OFF]`, the same pattern this
/// workspace uses for every protocol parameter the specs leave to
/// implementations.
pub const DEFAULT_VOTING_PERIOD: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// §15: "Quorum requirements are governance configurable" — the number of
/// distinct voters (not weight) required before a result counts, a
/// placeholder default `[PROPOSED — NEEDS SIGN-OFF]`.
pub const MINIMUM_VOTERS_FOR_QUORUM: usize = 3;
