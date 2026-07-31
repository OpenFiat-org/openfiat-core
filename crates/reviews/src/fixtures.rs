//! Fixtures shared by this crate's own tests.
//!
//! Deterministic keypairs rather than generated ones, so a failing
//! assertion names the same wallet on every run and a test that depends
//! on ordering does not pass or fail by luck of the draw.

use crate::record::{Rating, Review};
use openfiat_crypto::Keypair;
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_reservations::ReservationId;
use openfiat_settlement::{Settlement, SettlementId, SettlementState};
use openfiat_types::{Amount, PeerId, Timestamp};

/// Seed 1 is the buyer, 2 the seller, 9 a wallet in neither role.
pub fn keypair(seed: u8) -> Keypair {
    Keypair::from_seed([seed; 32])
}

pub fn peer(seed: u8) -> PeerId {
    peer_id_from_public_key(&keypair(seed).public_key()).expect("a seeded key derives a peer id")
}

pub fn other() -> PeerId {
    peer(9)
}

pub fn settlement_in(state: SettlementState) -> Settlement {
    let mut settlement = settled_settlement("s-1");
    settlement.state = state;
    settlement
}

pub fn settled_settlement(id: &str) -> Settlement {
    Settlement {
        id: SettlementId::new(id),
        reservation_id: ReservationId::new(format!("res-{id}")),
        buyer: peer(1),
        buyer_public_key: keypair(1).public_key(),
        seller: peer(2),
        seller_public_key: keypair(2).public_key(),
        amount: Amount::new(1_000_000, 6),
        state: SettlementState::Completed,
        payment_reference: None,
        escrow_release_signature: Some("onchain-sig".to_string()),
        payment_submitted_at: Some(Timestamp::from_millis(10)),
        merchant_responded_at: Some(Timestamp::from_millis(20)),
        payment_discrepancy: None,
        created_at: Timestamp::from_millis(1),
        updated_at: Timestamp::from_millis(30),
    }
}

pub fn review(settlement: &str, author_seed: u8, rating: Rating, comment: &str) -> Review {
    review_at(settlement, author_seed, rating, comment, 1_000)
}

pub fn review_at(
    settlement: &str,
    author_seed: u8,
    rating: Rating,
    comment: &str,
    at_millis: u64,
) -> Review {
    Review {
        settlement: SettlementId::new(settlement),
        author: peer(author_seed),
        author_public_key: keypair(author_seed).public_key(),
        rating,
        comment: comment.to_string(),
        created_at: Timestamp::from_millis(at_millis),
    }
}
