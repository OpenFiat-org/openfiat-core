//! The Phase 6b exit criterion for reputation: a score that moves in
//! response to simulated trade completions and a dispute loss, computed
//! purely from Reservations'/Settlement's/Disputes' own local state (no
//! gossip network needed — see the `view` module doc for why).

use openfiat_advertisements::events::{AdvertisementCreate, SignedAdvertisementCreate};
use openfiat_advertisements::{AdvertisementId, AdvertisementRegistry, Direction, PricingModel};
use openfiat_crypto::Keypair;
use openfiat_disputes::commitment;
use openfiat_disputes::events::{ArbitratorJoin, DisputeOpen, SignedArbitratorJoin, SignedDisputeOpen, SignedVoteCommit, SignedVoteReveal, VoteCommit, VoteReveal};
use openfiat_disputes::{DisputeId, DisputeRegistry, Vote};
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_reputation::{MerchantTier, ReputationView};
use openfiat_reservations::events::{ReservationRequest, SignedReservationRequest};
use openfiat_reservations::{ReservationId, ReservationRegistry};
use openfiat_settlement::events::{PaymentSubmitted, SettlementApproved, SettlementInitiate, SignedPaymentSubmitted, SignedSettlementApproved, SignedSettlementInitiate};
use openfiat_settlement::{SettlementId, SettlementRegistry};
use openfiat_storage::mem::MemoryStore;
use openfiat_types::{Amount, Timestamp};
use std::rc::Rc;
use std::time::Duration;

#[test]
fn a_merchant_profile_reflects_completed_trades_and_a_lost_dispute() {
    let advertisements = Rc::new(AdvertisementRegistry::new(MemoryStore::new()));
    let reservations = Rc::new(ReservationRegistry::new(MemoryStore::new(), Rc::clone(&advertisements)));
    let settlements = Rc::new(SettlementRegistry::new(MemoryStore::new()));
    let disputes = Rc::new(DisputeRegistry::new(MemoryStore::new(), Rc::clone(&settlements)));
    let reputation = ReputationView::new(Rc::clone(&reservations), Rc::clone(&settlements), Rc::clone(&disputes));

    let merchant = Keypair::generate();
    let merchant_id = peer_id_from_public_key(&merchant.public_key()).unwrap();
    let ad_id = AdvertisementId::new("ad-1");
    advertisements
        .apply_create(SignedAdvertisementCreate::sign(
            AdvertisementCreate {
                id: ad_id.clone(),
                merchant: merchant_id.clone(),
                merchant_public_key: merchant.public_key(),
                asset: "USDC".to_string(),
                direction: Direction::Sell,
                fiat_currency: "KES".to_string(),
                min_trade: Amount::new(1_000_000, 6),
                max_trade: Amount::new(5_000_000, 6),
                initial_liquidity: Amount::new(50_000_000, 6),
                pricing: PricingModel::Fixed { price: Amount::new(129_000_000, 6) },
                payment_methods: vec!["Mobile Money".to_string()],
                timestamp: Timestamp::now(),
            },
            &merchant,
        ))
        .unwrap();

    // Two buyers each complete a trade against the merchant.
    let complete_trade = |buyer: &Keypair, reservation_id: &str, settlement_id: &str, amount: Amount| {
        let buyer_id = peer_id_from_public_key(&buyer.public_key()).unwrap();
        let reservation = ReservationRequest {
            id: ReservationId::new(reservation_id),
            advertisement_id: ad_id.clone(),
            requester: buyer_id.clone(),
            requester_public_key: buyer.public_key(),
            amount,
            timestamp: Timestamp::now(),
        };
        reservations.apply_request(SignedReservationRequest::sign(reservation, buyer)).unwrap();

        let settlement_id = SettlementId::new(settlement_id);
        settlements
            .apply_initiate(SignedSettlementInitiate::sign(
                SettlementInitiate {
                    id: settlement_id.clone(),
                    reservation_id: ReservationId::new(reservation_id),
                    buyer: buyer_id.clone(),
                    buyer_public_key: buyer.public_key(),
                    seller: merchant_id.clone(),
                    seller_public_key: merchant.public_key(),
                    amount,
                    timestamp: Timestamp::now(),
                },
                buyer,
            ))
            .unwrap();
        settlements
            .apply_payment_submitted(SignedPaymentSubmitted::sign(
                PaymentSubmitted { settlement_id: settlement_id.clone(), buyer: buyer_id, payment_reference: Some("TXN".to_string()), timestamp: Timestamp::now() },
                buyer,
            ))
            .unwrap();
        settlements
            .apply_approved(SignedSettlementApproved::sign(SettlementApproved { settlement_id: settlement_id.clone(), seller: merchant_id.clone(), timestamp: Timestamp::now() }, &merchant))
            .unwrap();
        settlement_id
    };

    let buyer1 = Keypair::generate();
    let buyer2 = Keypair::generate();
    complete_trade(&buyer1, "res-1", "settle-1", Amount::new(2_000_000, 6));
    complete_trade(&buyer2, "res-2", "settle-2", Amount::new(3_000_000, 6));

    // A third trade the merchant loses a dispute over.
    let buyer3 = Keypair::generate();
    let buyer3_id = peer_id_from_public_key(&buyer3.public_key()).unwrap();
    let disputed_settlement_id = SettlementId::new("settle-3");
    reservations
        .apply_request(SignedReservationRequest::sign(
            ReservationRequest {
                id: ReservationId::new("res-3"),
                advertisement_id: ad_id.clone(),
                requester: buyer3_id.clone(),
                requester_public_key: buyer3.public_key(),
                amount: Amount::new(1_000_000, 6),
                timestamp: Timestamp::now(),
            },
            &buyer3,
        ))
        .unwrap();
    settlements
        .apply_initiate(SignedSettlementInitiate::sign(
            SettlementInitiate {
                id: disputed_settlement_id.clone(),
                reservation_id: ReservationId::new("res-3"),
                buyer: buyer3_id.clone(),
                buyer_public_key: buyer3.public_key(),
                seller: merchant_id.clone(),
                seller_public_key: merchant.public_key(),
                amount: Amount::new(1_000_000, 6),
                timestamp: Timestamp::now(),
            },
            &buyer3,
        ))
        .unwrap();

    let dispute_id = DisputeId::new("dispute-1");
    disputes
        .apply_open(SignedDisputeOpen::sign(
            DisputeOpen { id: dispute_id.clone(), settlement_id: disputed_settlement_id, opener: buyer3_id.clone(), opener_public_key: buyer3.public_key(), reason: "no payment".to_string(), timestamp: Timestamp::now() },
            &buyer3,
        ))
        .unwrap();
    let arbitrators: Vec<Keypair> = (0..3).map(|_| Keypair::generate()).collect();
    for arbitrator in &arbitrators {
        let join = ArbitratorJoin { dispute_id: dispute_id.clone(), arbitrator: peer_id_from_public_key(&arbitrator.public_key()).unwrap(), arbitrator_public_key: arbitrator.public_key(), timestamp: Timestamp::now() };
        disputes.apply_arbitrator_join(SignedArbitratorJoin::sign(join, arbitrator)).unwrap();
    }
    let secret = [7u8; 32];
    for arbitrator in &arbitrators {
        let commit =
            VoteCommit { dispute_id: dispute_id.clone(), arbitrator: peer_id_from_public_key(&arbitrator.public_key()).unwrap(), commitment: commitment::compute(Vote::BuyerWins, &secret), timestamp: Timestamp::now() };
        disputes.apply_vote_commit(SignedVoteCommit::sign(commit, arbitrator)).unwrap();
    }
    for arbitrator in &arbitrators {
        let reveal = VoteReveal { dispute_id: dispute_id.clone(), arbitrator: peer_id_from_public_key(&arbitrator.public_key()).unwrap(), vote: Vote::BuyerWins, secret, timestamp: Timestamp::now() };
        disputes.apply_vote_reveal(SignedVoteReveal::sign(reveal, arbitrator)).unwrap();
    }

    let profile = reputation.profile(&merchant_id);
    assert_eq!(profile.trades_started, 3);
    assert_eq!(profile.trades_completed, 2);
    assert_eq!(profile.disputes_involved, 1);
    assert_eq!(profile.disputes_lost, 1);
    assert_eq!(profile.total_volume, vec![Amount::new(5_000_000, 6)]);
    assert_eq!(profile.trade_success_rate(), Some(2.0 / 3.0));
    assert_eq!(profile.dispute_rate(), Some(0.5));
    assert_eq!(profile.tier(), MerchantTier::Explorer);

    // A buyer whose reservation goes stale shows up as a missed
    // reservation on their own profile, not the merchant's.
    let buyer4 = Keypair::generate();
    let buyer4_id = peer_id_from_public_key(&buyer4.public_key()).unwrap();
    reservations
        .apply_request(SignedReservationRequest::sign(
            ReservationRequest { id: ReservationId::new("res-4"), advertisement_id: ad_id, requester: buyer4_id.clone(), requester_public_key: buyer4.public_key(), amount: Amount::new(1_000_000, 6), timestamp: Timestamp::from_millis(1_000) },
            &buyer4,
        ))
        .unwrap();
    reservations.expire_stale(Duration::from_secs(0));

    let buyer4_profile = reputation.profile(&buyer4_id);
    assert_eq!(buyer4_profile.reservations_missed, 1);
    assert_eq!(buyer4_profile.trades_started, 0);
}
