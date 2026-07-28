//! Wire-level constants. Event names follow OFS-2400 §21 where it names
//! one; `MutualSettlementAgreed` isn't in that list (mutual settlement is
//! described in §17 but never given its own event name), so it's a
//! reasonable gap-fill rather than an invented replacement for something
//! the spec already names.

pub const OFS_SPEC: u16 = 2400;

pub const EVENT_OPENED: &str = "DisputeOpened";
pub const EVENT_ARBITRATOR_JOINED: &str = "ArbitratorJoined";
pub const EVENT_VOTE_COMMITTED: &str = "VoteCommitted";
pub const EVENT_VOTE_REVEALED: &str = "VoteRevealed";
pub const EVENT_MUTUAL_SETTLEMENT_AGREED: &str = "MutualSettlementAgreed";

/// §16, Ch.11 §11.9 (simplified for MVP — see the `record` module doc):
/// how many arbitrators must join before a case locks.
pub const REQUIRED_ARBITRATORS: u8 = 3;
