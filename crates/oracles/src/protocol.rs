//! Wire-level constants. OFS-7000 §8's publication lifecycle never
//! names a PascalCase event (it's a generic narrative diagram: Collect
//! Data → Sign Update → Publish → ...), and OFS-8100 (OETR)'s own Oracle
//! Events vocabulary (`OraclePriceUpdated`, `OracleFeedStarted`, ...) is
//! price-feed-specific — it doesn't cleanly cover this crate's other
//! three categories (stablecoin/payment/regional metadata). Rather than
//! force a mismatched name onto non-price records, this crate mints one
//! event name in OETR's own PascalCase convention that covers every
//! category uniformly.

pub const OFS_SPEC: u16 = 7000;

pub const EVENT_PUBLISHED: &str = "OracleRecordPublished";
