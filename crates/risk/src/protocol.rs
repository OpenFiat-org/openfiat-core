//! Wire-level constants. Event names follow OFS-8100 (OETR)'s Risk
//! Events vocabulary directly — unlike `openfiat-oracles`, OETR's `RSK`
//! category already covers this crate's exact two outcomes.

pub const OFS_SPEC: u16 = 7100;

pub const EVENT_FLAGGED: &str = "WalletFlagged";
pub const EVENT_CLEARED: &str = "WalletCleared";
