//! Wire-level constants.
//!
//! # Why these events ride OFS-2300
//!
//! No published OFS specification defines a confidential trade channel.
//! Every other domain crate here takes its spec number from a document
//! that exists; minting a fresh one for a document that does not would be
//! a claim this workspace cannot back, and it would put a number into
//! `SUPPORTED_OFS` that no integrator could look up.
//!
//! So these events travel under the spec of the record they hang off: a
//! channel exists only for a settlement, is authorized entirely by that
//! settlement's parties, and dies with it. Event *names* are namespaced
//! instead (`TradeChannel…`), and both registries filtering on OFS-2300
//! ignore each other's event types the same way every `apply_event` in
//! this workspace ignores what it does not recognise.

pub const OFS_SPEC: u16 = openfiat_settlement::protocol::OFS_SPEC;

pub const EVENT_KEY_GRANTED: &str = "TradeChannelKeyGranted";
pub const EVENT_ENTRY_POSTED: &str = "TradeChannelEntryPosted";

/// The column families this crate writes.
///
/// Exported because a column family missing from the set a node opens its
/// database with does not fail loudly — `KvStore::put` returns an error
/// the registries deliberately swallow (they must: a write failure on a
/// gossiped event cannot be allowed to stop the event loop), so every
/// write is lost in silence while every `MemoryStore` test passes. This
/// list is what `openfiat_rpc::SNAPSHOT_COLUMN_FAMILIES` is checked
/// against, so adding one here cannot be forgotten there.
///
/// `tests/column_families.rs` demonstrates the failure mode against a
/// real RocksDB rather than describing it.
pub const COLUMN_FAMILIES: &[&str] = &[
    crate::store::GRANTS_COLUMN_FAMILY,
    crate::store::ENTRIES_COLUMN_FAMILY,
];
