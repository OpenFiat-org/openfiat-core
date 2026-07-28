//! An end-to-end proof that the four marketplace-core crates actually
//! compose the way OFS-2000 §9 describes: an advertisement, a
//! reservation against it, and a settlement of that reservation, viewed
//! together as one `Trade` with the right aggregate status at each step.
//! Applied directly against the registries (no gossip network) — each
//! crate's own replication is already proven by its own integration test.

use openfiat_advertisements::events::{AdvertisementCreate, SignedAdvertisementCreate};
use openfiat_advertisements::{AdvertisementId, AdvertisementRegistry, Direction, PricingModel};
use openfiat_crypto::Keypair;
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_reservations::events::{ReservationRequest, SignedReservationRequest};
use openfiat_reservations::ReservationRegistry;
use openfiat_settlement::events::{PaymentSubmitted, SettlementApproved, SettlementInitiate, SignedPaymentSubmitted, SignedSettlementApproved, SignedSettlementInitiate};
use openfiat_settlement::SettlementRegistry;
use openfiat_storage::mem::MemoryStore;
use openfiat_trade::{TradeStatus, TradeView};
use openfiat_types::{Amount, Timestamp};
use std::rc::Rc;

#[test]
fn a_trade_progresses_through_the_expected_aggregate_statuses() {
    let advertisements = Rc::new(AdvertisementRegistry::new(MemoryStore::new()));
    let reservations = Rc::new(ReservationRegistry::new(MemoryStore::new(), Rc::clone(&advertisements)));
    let settlements = Rc::new(SettlementRegistry::new(MemoryStore::new()));
    let trades = TradeView::new(Rc::clone(&reservations), Rc::clone(&settlements));

    let merchant = Keypair::generate();
    let buyer = Keypair::generate();
    let ad_id = AdvertisementId::new("ad-1");
    let create = AdvertisementCreate {
        id: ad_id.clone(),
        merchant: peer_id_from_public_key(&merchant.public_key()).unwrap(),
        merchant_public_key: merchant.public_key(),
        asset: "USDC".to_string(),
        direction: Direction::Sell,
        fiat_currency: "KES".to_string(),
        min_trade: Amount::new(1_000_000, 6),
        max_trade: Amount::new(5_000_000, 6),
        initial_liquidity: Amount::new(10_000_000, 6),
        pricing: PricingModel::Fixed { price: Amount::new(129_000_000, 6) },
        payment_methods: vec!["Mobile Money".to_string()],
        timestamp: Timestamp::now(),
    };
    advertisements.apply_create(SignedAdvertisementCreate::sign(create, &merchant)).unwrap();

    let reservation_request = ReservationRequest {
        id: openfiat_reservations::ReservationId::new("res-1"),
        advertisement_id: ad_id,
        requester: peer_id_from_public_key(&buyer.public_key()).unwrap(),
        requester_public_key: buyer.public_key(),
        amount: Amount::new(2_000_000, 6),
        timestamp: Timestamp::now(),
    };
    let reservation_id = reservations.apply_request(SignedReservationRequest::sign(reservation_request, &buyer)).unwrap();

    // Reservation exists, no settlement started yet.
    let trade = trades.get(&reservation_id).unwrap();
    assert_eq!(trade.status(), TradeStatus::EscrowLocked);
    assert!(trade.settlement.is_none());

    let settlement_id = openfiat_settlement::SettlementId::new("settle-1");
    let initiate = SettlementInitiate {
        id: settlement_id.clone(),
        reservation_id: reservation_id.clone(),
        buyer: peer_id_from_public_key(&buyer.public_key()).unwrap(),
        buyer_public_key: buyer.public_key(),
        seller: peer_id_from_public_key(&merchant.public_key()).unwrap(),
        seller_public_key: merchant.public_key(),
        amount: Amount::new(2_000_000, 6),
        timestamp: Timestamp::now(),
    };
    settlements.apply_initiate(SignedSettlementInitiate::sign(initiate, &buyer)).unwrap();

    let trade = trades.get(&reservation_id).unwrap();
    assert_eq!(trade.status(), TradeStatus::AwaitingPayment);

    settlements
        .apply_payment_submitted(SignedPaymentSubmitted::sign(
            PaymentSubmitted { settlement_id: settlement_id.clone(), buyer: peer_id_from_public_key(&buyer.public_key()).unwrap(), payment_reference: Some("TXN1".to_string()), timestamp: Timestamp::now() },
            &buyer,
        ))
        .unwrap();
    assert_eq!(trades.get(&reservation_id).unwrap().status(), TradeStatus::PaymentSubmitted);

    settlements
        .apply_approved(SignedSettlementApproved::sign(SettlementApproved { settlement_id, seller: peer_id_from_public_key(&merchant.public_key()).unwrap(), timestamp: Timestamp::now() }, &merchant))
        .unwrap();
    let trade = trades.get(&reservation_id).unwrap();
    assert_eq!(trade.status(), TradeStatus::Completed);

    // The whole cluster's trade list contains exactly this one, fully joined.
    let all = trades.all();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].reservation.id, reservation_id);
    assert!(all[0].settlement.is_some());
}
