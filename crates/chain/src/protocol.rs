//! Wire-level constants (OFS-4300; event names registered in OFS-8100 §18).

pub const OFS_SPEC: u16 = 4300;

pub const EVENT_BLOCKHASH_ANNOUNCED: &str = "BlockhashAnnounced";
pub const EVENT_TRANSACTION_RELAY_REQUESTED: &str = "TransactionRelayRequested";
pub const EVENT_TRANSACTION_RELAYED: &str = "TransactionRelayed";
