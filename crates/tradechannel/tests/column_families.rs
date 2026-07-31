//! Proves the failure mode a new column family introduces, against a
//! real RocksDB rather than a `MemoryStore`.
//!
//! `MemoryStore` creates a column family on first write, so a registry
//! writing to one nobody opened works perfectly in every unit test in
//! this workspace. `Database` does not: `KvStore::put` returns
//! `UnknownColumnFamily`, and every registry here deliberately discards
//! that error — it must, because a write failure while applying a
//! gossiped event cannot be allowed to stop the event loop. The result is
//! that a column family missing from the node's open list loses every
//! write in complete silence on a real node while the whole test suite
//! stays green.
//!
//! That has already happened once in this project. These two tests make
//! it impossible to happen quietly a second time for this crate: one
//! demonstrates the loss, the other pins the list that prevents it.

use openfiat_crypto::Keypair;
use openfiat_database::Database;
use openfiat_disputes::DisputeRegistry;
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_reservations::ReservationId;
use openfiat_settlement::events::{SettlementInitiate, SignedSettlementInitiate};
use openfiat_settlement::{SettlementId, SettlementRegistry};
use openfiat_storage::KvStore;
use openfiat_tradechannel::events::{SignedTradeChannelEntryPost, TradeChannelEntryPost};
use openfiat_tradechannel::{
    COLUMN_FAMILIES, ChannelKey, EntryBinding, EntryKind, TradeChannelRegistry, seal_entry,
};
use openfiat_types::{Amount, PeerId, Timestamp};
use std::rc::Rc;

/// Everything a node opens for these tests apart from this crate's own
/// families — enough for the settlement registry the channel checks
/// against, so the only variable is whether the channel's families are
/// present.
const OTHER_FAMILIES: &[&str] = &["settlements", "disputes"];

fn settlement_id() -> SettlementId {
    SettlementId::new("settle-1")
}

fn peer(keypair: &Keypair) -> PeerId {
    peer_id_from_public_key(&keypair.public_key()).unwrap()
}

/// A registry over a real RocksDB opened with exactly `families`, with
/// one settlement already applied so an entry has something to hang off.
fn registry_over(
    directory: &std::path::Path,
    families: &[&str],
    buyer: &Keypair,
    seller: &Keypair,
) -> TradeChannelRegistry<Rc<Database>> {
    let database = Rc::new(Database::open(directory, families).expect("opens"));
    let settlements = Rc::new(SettlementRegistry::new(Rc::clone(&database)));
    settlements
        .apply_initiate(SignedSettlementInitiate::sign(
            SettlementInitiate {
                id: settlement_id(),
                reservation_id: ReservationId::new("res-1"),
                buyer: peer(buyer),
                buyer_public_key: buyer.public_key(),
                seller: peer(seller),
                seller_public_key: seller.public_key(),
                amount: Amount::new(2_000_000, 6),
                timestamp: Timestamp::now(),
            },
            buyer,
        ))
        .expect("the settlements family is always opened here");
    let disputes = Rc::new(DisputeRegistry::new(
        Rc::clone(&database),
        Rc::clone(&settlements),
    ));
    TradeChannelRegistry::new(database, settlements, disputes)
}

fn signed_entry(seller: &Keypair) -> SignedTradeChannelEntryPost {
    let key = ChannelKey::generate();
    let author = peer(seller);
    let id = settlement_id();
    let payload = seal_entry(
        &key,
        &EntryBinding {
            settlement_id: &id,
            author: &author,
            sequence: 0,
            kind: EntryKind::PaymentDetails.name(),
        },
        b"Equity Bank 0110123456789",
    )
    .unwrap();
    SignedTradeChannelEntryPost::sign(
        TradeChannelEntryPost {
            settlement_id: id,
            author,
            sequence: 0,
            kind: EntryKind::PaymentDetails,
            payload,
            timestamp: Timestamp::now(),
        },
        seller,
    )
}

/// The bug, reproduced. `apply_entry` reports success — it verified the
/// signature and the authorship, which is all it claims to do — and the
/// write vanishes.
#[test]
fn a_missing_column_family_loses_every_write_without_reporting_an_error() {
    let directory = tempfile::tempdir().unwrap();
    let buyer = Keypair::generate();
    let seller = Keypair::generate();
    let registry = registry_over(directory.path(), OTHER_FAMILIES, &buyer, &seller);

    registry
        .apply_entry(signed_entry(&seller))
        .expect("verification passes; only the write fails, and silently");

    assert!(
        registry.channel(&settlement_id()).entries.is_empty(),
        "this is the failure being demonstrated: a node whose database was \
         opened without this crate's column families accepts every entry \
         and stores none of them"
    );
}

/// The same sequence with the families this crate exports. Together with
/// the test above, this is what makes `COLUMN_FAMILIES` load-bearing
/// rather than documentation.
#[test]
fn the_exported_column_families_are_exactly_what_this_crate_needs_to_persist() {
    let directory = tempfile::tempdir().unwrap();
    let buyer = Keypair::generate();
    let seller = Keypair::generate();
    let families: Vec<&str> = OTHER_FAMILIES
        .iter()
        .chain(COLUMN_FAMILIES)
        .copied()
        .collect();
    let registry = registry_over(directory.path(), &families, &buyer, &seller);

    registry.apply_entry(signed_entry(&seller)).unwrap();
    assert_eq!(registry.channel(&settlement_id()).entries.len(), 1);
}

/// A node that opened the right families must also be able to *read* them
/// back after a restart, which is the only thing that distinguishes a
/// persisted channel from a cache.
#[test]
fn a_channel_survives_reopening_the_database() {
    let directory = tempfile::tempdir().unwrap();
    let buyer = Keypair::generate();
    let seller = Keypair::generate();
    let families: Vec<&str> = OTHER_FAMILIES
        .iter()
        .chain(COLUMN_FAMILIES)
        .copied()
        .collect();

    {
        let registry = registry_over(directory.path(), &families, &buyer, &seller);
        registry.apply_entry(signed_entry(&seller)).unwrap();
    }

    let database = Rc::new(Database::open(directory.path(), &families).expect("reopens"));
    let settlements = Rc::new(SettlementRegistry::new(Rc::clone(&database)));
    let disputes = Rc::new(DisputeRegistry::new(
        Rc::clone(&database),
        Rc::clone(&settlements),
    ));
    let registry = TradeChannelRegistry::new(Rc::clone(&database), settlements, disputes);
    assert_eq!(registry.channel(&settlement_id()).entries.len(), 1);

    // And the stored bytes are still ciphertext on disk, not something a
    // restart quietly turned into plaintext.
    let stored = database
        .iter_prefix("trade_channel_entries", &[])
        .unwrap()
        .into_iter()
        .flat_map(|(_, value)| value)
        .collect::<Vec<u8>>();
    assert!(
        !stored.windows(13).any(|window| window == b"0110123456789"),
        "the account number must not be recoverable from the node's disk"
    );
}
