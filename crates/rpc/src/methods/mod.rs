//! Every method this node exposes, one module per domain crate. Each
//! module's `register` adds its own `getX`/`sendX` entries to the shared
//! [`MethodTable`] — see `dispatch` for the `getX`/`sendX` shape itself.

pub mod advertisements;
pub mod chain;
pub mod disputes;
pub mod governance;
pub mod identity;
pub mod node;
pub mod notifications;
pub mod oracles;
pub mod providers;
pub mod reputation;
pub mod reservations;
pub mod risk;
pub mod sessions;
pub mod settlement;
pub mod snapshot;
pub mod trade;

use crate::dispatch::MethodTable;
use openfiat_storage::KvStore;

pub fn build_table<S: KvStore + 'static>() -> MethodTable<S> {
    let mut table = MethodTable::new();
    node::register(&mut table);
    advertisements::register(&mut table);
    reservations::register(&mut table);
    settlement::register(&mut table);
    trade::register(&mut table);
    disputes::register(&mut table);
    identity::register(&mut table);
    reputation::register(&mut table);
    governance::register(&mut table);
    providers::register(&mut table);
    notifications::register(&mut table);
    oracles::register(&mut table);
    risk::register(&mut table);
    snapshot::register(&mut table);
    sessions::register(&mut table);
    chain::register(&mut table);
    table
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfiat_storage::mem::MemoryStore;

    #[test]
    fn every_method_registers_without_a_duplicate_name_panic() {
        let table: MethodTable<MemoryStore> = build_table();
        // 15 domains registered above; a real assertion (not just "it
        // didn't panic") that the table actually has entries.
        assert!(table.method_names().len() > 15);
    }
}
