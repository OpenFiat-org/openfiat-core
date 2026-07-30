//! A real advertisement → reservation → settlement → trade flow, chained
//! across three domain crates and converging on a genuine 3-node gossip
//! cluster. Each domain's own `tests/replication.rs` only proves *that*
//! domain's events converge in isolation; nothing before this test chains
//! them the way a real trade actually does (OFS-2000 §9's two-phase
//! lifecycle: Reservation, then Settlement).

use openfiat_advertisements::events::{AdvertisementCreate, SignedAdvertisementCreate};
use openfiat_advertisements::record::{Direction, PricingModel};
use openfiat_advertisements::{AdvertisementId, protocol as adv_protocol};
use openfiat_conformance::spawn_cluster;
use openfiat_crypto::{Keypair, MintAddress};
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_reservations::events::{ReservationRequest, SignedReservationRequest};
use openfiat_reservations::{ReservationId, protocol as rsv_protocol};
use openfiat_settlement::events::{
    PaymentSubmitted, SettlementApproved, SettlementInitiate, SignedPaymentSubmitted,
    SignedSettlementApproved, SignedSettlementInitiate,
};
use openfiat_settlement::{SettlementId, protocol as set_protocol};
use openfiat_trade::TradeStatus;
use openfiat_types::{Amount, NodeRole, Priority, Timestamp};

#[tokio::test]
async fn a_trade_completes_end_to_end_and_converges_across_the_cluster() {
    // Node 0: merchant (needs MerchantGateway to originate ad events, per
    // OGP §7's role-scoped authorization). Node 1: buyer. Node 2: a third
    // node with no stake in the trade, proving convergence isn't an
    // artifact of only the two participants driving their loops.
    let roles = vec![vec![NodeRole::MerchantGateway], vec![], vec![]];
    let mut nodes = spawn_cluster(openfiat_storage::mem::MemoryStore::new, &roles).await;

    let merchant = Keypair::from_seed([1u8; 32]);
    let buyer = Keypair::from_seed([2u8; 32]);
    let merchant_peer = peer_id_from_public_key(&merchant.public_key()).unwrap();
    let buyer_peer = peer_id_from_public_key(&buyer.public_key()).unwrap();

    // --- Advertisement (merchant, node 0) ---
    let ad_id = AdvertisementId::new("ad-1");
    let create = AdvertisementCreate {
        id: ad_id.clone(),
        merchant: merchant_peer.clone(),
        merchant_public_key: merchant.public_key(),
        asset_mint: MintAddress::parse("C4rSGhdxWhSFQuFcAxQti1JvBxriwHJoHtJjfhs5p24Y").unwrap(),
        direction: Direction::Sell,
        fiat_currency: "PHP".to_string(),
        min_trade: Amount::new(10_00, 2),
        max_trade: Amount::new(100_000, 2),
        initial_liquidity: Amount::new(100_000, 2),
        pricing: PricingModel::Fixed {
            price: Amount::new(56_50, 2),
        },
        payment_methods: vec!["GCash".to_string()],
        timestamp: Timestamp::now(),
    };
    let signed_ad = SignedAdvertisementCreate::sign(create, &merchant);
    nodes[0]
        .originate(
            adv_protocol::EVENT_CREATED,
            adv_protocol::OFS_SPEC,
            Priority::Advertisement,
            8,
            &signed_ad,
        )
        .unwrap();

    openfiat_conformance::drive_until(&mut nodes, |nodes| {
        nodes.iter().all(|n| n.advertisements.get(&ad_id).is_some())
    })
    .await;

    // --- Reservation (buyer, node 1) ---
    let reservation_id = ReservationId::new("res-1");
    let request = ReservationRequest {
        id: reservation_id.clone(),
        advertisement_id: ad_id.clone(),
        requester: buyer_peer.clone(),
        requester_public_key: buyer.public_key(),
        amount: Amount::new(50_00, 2),
        agreed_price: Amount::new(56_50, 2),
        agreed_mid: None,
        timestamp: Timestamp::now(),
    };
    let signed_request = SignedReservationRequest::sign(request, &buyer);
    nodes[1]
        .originate(
            rsv_protocol::EVENT_REQUESTED,
            rsv_protocol::OFS_SPEC,
            Priority::SessionReservationSettlement,
            8,
            &signed_request,
        )
        .unwrap();

    openfiat_conformance::drive_until(&mut nodes, |nodes| {
        nodes
            .iter()
            .all(|n| n.reservations.get(&reservation_id).is_some())
    })
    .await;

    // Every node — including the merchant, who only learned about it via
    // gossip — sees the same escrow-locked reservation reducing the same
    // advertisement's available liquidity (§9-10).
    for node in &nodes {
        assert_eq!(
            node.advertisements
                .get(&ad_id)
                .unwrap()
                .available_liquidity
                .base_units(),
            95_000
        );
    }

    // --- Settlement (buyer initiates + pays, merchant approves) ---
    let settlement_id = SettlementId::new("set-1");
    let initiate = SettlementInitiate {
        id: settlement_id.clone(),
        reservation_id: reservation_id.clone(),
        buyer: buyer_peer.clone(),
        buyer_public_key: buyer.public_key(),
        seller: merchant_peer.clone(),
        seller_public_key: merchant.public_key(),
        amount: Amount::new(50_00, 2),
        timestamp: Timestamp::now(),
    };
    let signed_initiate = SignedSettlementInitiate::sign(initiate, &buyer);
    nodes[1]
        .originate(
            set_protocol::EVENT_INITIATED,
            set_protocol::OFS_SPEC,
            Priority::SessionReservationSettlement,
            8,
            &signed_initiate,
        )
        .unwrap();

    openfiat_conformance::drive_until(&mut nodes, |nodes| {
        nodes
            .iter()
            .all(|n| n.settlements.get(&settlement_id).is_some())
    })
    .await;

    let payment = PaymentSubmitted {
        settlement_id: settlement_id.clone(),
        buyer: buyer_peer.clone(),
        payment_reference: Some("GCASH-REF-001".to_string()),
        timestamp: Timestamp::now(),
    };
    let signed_payment = SignedPaymentSubmitted::sign(payment, &buyer);
    nodes[1]
        .originate(
            set_protocol::EVENT_PAYMENT_SUBMITTED,
            set_protocol::OFS_SPEC,
            Priority::SessionReservationSettlement,
            8,
            &signed_payment,
        )
        .unwrap();

    openfiat_conformance::drive_until(&mut nodes, |nodes| {
        nodes.iter().all(|n| {
            n.trades.get(&reservation_id).unwrap().status() == TradeStatus::PaymentSubmitted
        })
    })
    .await;

    let approved = SettlementApproved {
        settlement_id: settlement_id.clone(),
        seller: merchant_peer.clone(),
        timestamp: Timestamp::now(),
    };
    let signed_approved = SignedSettlementApproved::sign(approved, &merchant);
    nodes[0]
        .originate(
            set_protocol::EVENT_APPROVED,
            set_protocol::OFS_SPEC,
            Priority::SessionReservationSettlement,
            8,
            &signed_approved,
        )
        .unwrap();

    openfiat_conformance::drive_until(&mut nodes, |nodes| {
        nodes
            .iter()
            .all(|n| n.trades.get(&reservation_id).unwrap().status() == TradeStatus::Completed)
    })
    .await;

    // The observer node (2), which never originated anything, ends up
    // with the exact same trade view purely from gossip — the actual
    // point of this test.
    let observer_trade = nodes[2].trades.get(&reservation_id).unwrap();
    assert_eq!(observer_trade.status(), TradeStatus::Completed);
    assert_eq!(observer_trade.settlement.unwrap().id, settlement_id);
}
