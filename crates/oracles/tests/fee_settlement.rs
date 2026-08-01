//! Settling a USDC-denominated provider fee in OPEN, across nodes that do
//! not agree about the rate.
//!
//! `openfiat_registry::settlement` states the rule and proves it as a pure
//! function. This proves the property that actually matters on a network:
//! that nodes holding *genuinely different oracle records* — including a
//! node whose feed has lapsed and a `GossipOnly` node holding none at all
//! — reach the same verdict on the same signed commitment. If they did
//! not, the same payer would be honoured by one access node and turned
//! away by the next, which is the failure
//! `openfiat_reservations::ReservationRegistry::apply_request` already
//! refuses to introduce for a reservation's agreed price.

use openfiat_crypto::Keypair;
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_oracles::events::{OraclePublish, SignedOraclePublish};
use openfiat_oracles::{OracleData, OracleId, OracleIndex};
use openfiat_registry::pricing::{BillingUnit, ServicePricing};
use openfiat_registry::settlement::{
    FeeQuote, FeeSettlement, FeeSettlementError, SignedFeeSettlement, UnsettleableReason,
};
use openfiat_registry::{Registration, Registry, SignedRegistration};
use openfiat_storage::mem::MemoryStore;
use openfiat_types::{
    Amount, MarketDataService, NotificationChannel, ServiceId, ServiceType, Timestamp,
};
use std::rc::Rc;

const USDC: &str = "2bHPi5hA4zrmPAfrvLmEexg3KJjpTjNkUcxWnzUPeRRU";
const OPEN: &str = "29w8TroBTYoaqrXBDcpv5L54VZRA8Kf7kU5U1cakvFdj";
/// The OPEN mint is Token-2022 with nine decimals.
const OPEN_DECIMALS: u8 = 9;
const PAYOUT: &str = "EA8TyQ58C3eavg3ThRFTMu1KLyV9e1v2oTQubSBQ9s5z";

/// Inside the sixty-second life of the records published below, and well
/// outside the one-millisecond life of the lapsed node's.
const NOW: Timestamp = Timestamp::from_millis(30_000);

/// The fee-charging service, identically registered on every node — a
/// registration is signed once and replicated, so every node sees the same
/// declared price. 0.25 USDC per delivered notification.
fn fee_service(keypair: &Keypair) -> SignedRegistration {
    SignedRegistration::sign(
        Registration {
            service_id: ServiceId::new("gateway-1"),
            service_type: ServiceType::Notifications(NotificationChannel::Push),
            provider: peer_id_from_public_key(&keypair.public_key()).unwrap(),
            provider_public_key: keypair.public_key(),
            endpoints: vec![],
            supported_ofs: vec![1500, 4100],
            region: None,
            capabilities: vec![],
            branding: None,
            pricing: Some(ServicePricing {
                token_mint: USDC.to_string(),
                amount: Amount::new(250_000, 6),
                unit: BillingUnit::Request,
            }),
            payout_wallet: Some(PAYOUT.to_string()),
            timestamp: Timestamp::from_millis(1),
        },
        keypair,
    )
}

/// One node's local state: the replicated registry, and an oracle index
/// holding whatever USDC/OPEN records happened to reach *this* node.
struct Node {
    services: Rc<Registry<MemoryStore>>,
    oracles: OracleIndex<MemoryStore>,
}

impl Node {
    /// `feed` is the rate this node's own market-data provider published
    /// and how long it published it for. `None` is a node holding no
    /// USDC/OPEN record at all — a `GossipOnly` node, or any node before
    /// the first publication reaches it.
    fn new(gateway: &Keypair, market_data_seed: u8, feed: Option<(f64, u64)>) -> Self {
        let services = Rc::new(Registry::new(MemoryStore::new()));
        services
            .apply_registration(fee_service(gateway))
            .expect("the gateway registration is the same on every node");
        let oracles = OracleIndex::new(MemoryStore::new(), Rc::clone(&services));

        if let Some((rate, ttl_millis)) = feed {
            let publisher = Keypair::from_seed([market_data_seed; 32]);
            services
                .apply_registration(SignedRegistration::sign(
                    Registration {
                        service_id: ServiceId::new(format!("fx-{market_data_seed}")),
                        service_type: ServiceType::MarketData(MarketDataService::FxOracle),
                        provider: peer_id_from_public_key(&publisher.public_key()).unwrap(),
                        provider_public_key: publisher.public_key(),
                        endpoints: vec![],
                        supported_ofs: vec![7000],
                        region: None,
                        capabilities: vec!["USDC/OPEN".to_string()],
                        branding: None,
                        pricing: None,
                        payout_wallet: None,
                        timestamp: Timestamp::from_millis(1),
                    },
                    &publisher,
                ))
                .expect("a market-data provider registers like any other service");
            oracles
                .apply_publish(SignedOraclePublish::sign(
                    OraclePublish {
                        id: OracleId::new(format!("usdc-open-{market_data_seed}")),
                        provider: peer_id_from_public_key(&publisher.public_key()).unwrap(),
                        provider_public_key: publisher.public_key(),
                        data: OracleData::ExchangeRate {
                            base: "USDC".to_string(),
                            quote: "OPEN".to_string(),
                            rate,
                        },
                        version: 1,
                        timestamp: Timestamp::from_millis(1),
                        expires_at: Timestamp::from_millis(1 + ttl_millis),
                    },
                    &publisher,
                ))
                .expect("a registered market-data provider may publish");
        }
        Self { services, oracles }
    }

    fn pricing(&self) -> Option<ServicePricing> {
        self.services.get(&ServiceId::new("gateway-1"))?.pricing
    }

    /// Exactly what a node does when a payer asks what this fee costs in
    /// OPEN: read the pair, carry the read across, convert.
    fn quote(&self, now: Timestamp) -> FeeQuote {
        match self.pricing() {
            None => FeeQuote::Free,
            Some(pricing) => pricing.settle_in(
                OPEN,
                OPEN_DECIMALS,
                self.oracles
                    .exchange_rate("USDC", "OPEN", now)
                    .as_settlement_rate(),
                now,
            ),
        }
    }

    fn verdict(
        &self,
        signed: &SignedFeeSettlement,
        now: Timestamp,
    ) -> Result<(), FeeSettlementError> {
        signed.accepts(self.pricing().as_ref(), now)
    }
}

/// A payer signing the quote their own access node handed them.
fn commit(quote: FeeQuote, payer: &Keypair) -> SignedFeeSettlement {
    let FeeQuote::Settleable {
        fee,
        fee_mint,
        settlement_mint,
        settlement_amount,
        rate,
        quoted_at,
        expires_at,
    } = quote
    else {
        panic!("the quoting node must have had a live rate");
    };
    SignedFeeSettlement::sign(
        FeeSettlement {
            service_id: ServiceId::new("gateway-1"),
            payer: peer_id_from_public_key(&payer.public_key()).unwrap(),
            payer_public_key: payer.public_key(),
            fee,
            fee_mint,
            settlement_mint,
            settlement_amount,
            rate,
            quoted_at,
            expires_at,
            reference: "delivery-8814".to_string(),
        },
        payer,
    )
}

/// The headline property. Four nodes, four different oracle situations,
/// one signed commitment, one verdict.
#[test]
fn nodes_that_disagree_about_the_rate_agree_about_the_commitment() {
    let gateway = Keypair::from_seed([9; 32]);
    let payer = Keypair::generate();

    // Both honest, both current, ten percent apart — an ordinary amount of
    // disagreement between two feeds sampled seconds apart.
    let quoting = Node::new(&gateway, 1, Some((4.0, 60_000)));
    let other = Node::new(&gateway, 2, Some((4.4, 60_000)));
    // Its record lapsed long before `NOW`: the feed is published here and
    // dead.
    let lapsed = Node::new(&gateway, 3, Some((4.0, 1)));
    // Holds no USDC/OPEN record at all.
    let gossip_only = Node::new(&gateway, 4, None);

    // The divergence is real, not a fixture detail: each node's own quote
    // differs, and two of them have no quote to give.
    assert_eq!(
        quoting.quote(NOW).settlement_amount(),
        Some(Amount::new(1_000_000_000, 9)),
        "0.25 USDC at 4 OPEN per USDC is 1 OPEN"
    );
    assert_eq!(
        other.quote(NOW).settlement_amount(),
        Some(Amount::new(1_100_000_000, 9)),
        "the same fee is a different number on a node with a different record"
    );
    assert_eq!(
        lapsed.quote(NOW),
        FeeQuote::Unsettleable {
            reason: UnsettleableReason::StaleOracleData
        }
    );
    assert_eq!(
        gossip_only.quote(NOW),
        FeeQuote::Unsettleable {
            reason: UnsettleableReason::NoOracleData
        }
    );

    // One payer signs one of those quotes. Every node honours it, however
    // little its own records agree — including the two that could not have
    // produced a quote themselves.
    let signed = commit(quoting.quote(NOW), &payer);
    let at = Timestamp::from_millis(NOW.as_millis() + 5_000);
    for (label, node) in [
        ("the node that quoted it", &quoting),
        ("a node with a different rate", &other),
        ("a node whose feed has lapsed", &lapsed),
        ("a node holding no oracle records", &gossip_only),
    ] {
        assert_eq!(node.verdict(&signed, at), Ok(()), "{label} must honour it");
    }
}

/// The other half: refusal has to be unanimous too. A verdict that only
/// agreed on acceptance would still let a payer shop for a node.
#[test]
fn nodes_that_disagree_about_the_rate_agree_about_refusing_a_bad_commitment() {
    let gateway = Keypair::from_seed([9; 32]);
    let payer = Keypair::generate();
    let quoting = Node::new(&gateway, 1, Some((4.0, 60_000)));
    let other = Node::new(&gateway, 2, Some((4.4, 60_000)));
    let gossip_only = Node::new(&gateway, 4, None);

    // The rate the payer recorded, the arithmetic they wished were true.
    let mut tampered = commit(quoting.quote(NOW), &payer).settlement;
    tampered.settlement_amount = Amount::new(1, 9);
    let signed = SignedFeeSettlement::sign(tampered, &payer);

    let at = Timestamp::from_millis(NOW.as_millis() + 5_000);
    for node in [&quoting, &other, &gossip_only] {
        assert_eq!(
            node.verdict(&signed, at),
            Err(FeeSettlementError::PriceDisagreement)
        );
    }
}

/// The quote's window is the payer's option, so it has to close at the
/// same instant everywhere — including on the node whose own feed is still
/// perfectly current.
#[test]
fn every_node_lets_the_same_quote_expire_at_the_same_instant() {
    let gateway = Keypair::from_seed([9; 32]);
    let payer = Keypair::generate();
    let quoting = Node::new(&gateway, 1, Some((4.0, 60_000)));
    let other = Node::new(&gateway, 2, Some((4.4, 60_000)));
    let gossip_only = Node::new(&gateway, 4, None);

    let signed = commit(quoting.quote(NOW), &payer);
    let expires_at = signed.settlement.expires_at;
    for node in [&quoting, &other, &gossip_only] {
        assert_eq!(
            node.verdict(&signed, Timestamp::from_millis(expires_at.as_millis() - 1)),
            Ok(()),
            "live right up to the boundary"
        );
        assert_eq!(
            node.verdict(&signed, expires_at),
            Err(FeeSettlementError::QuoteExpired),
            "and past it the payer bears the movement and must re-quote"
        );
    }
}

/// A lapsed feed is refused, not quietly re-used. This is the same rule
/// `openfiat_oracles::OracleIndex::exchange_rate` already applies, followed
/// all the way through to the fee: there is no last-known rate anywhere on
/// this path, so "this fee cannot be settled in OPEN right now" is the
/// answer, and the fee stays payable in the USDC it was declared in.
#[test]
fn a_node_whose_feed_died_refuses_to_price_rather_than_reusing_the_last_rate() {
    let gateway = Keypair::from_seed([9; 32]);
    let node = Node::new(&gateway, 3, Some((4.0, 60_000)));

    // While the record is live it prices normally.
    let while_live = Timestamp::from_millis(30_000);
    assert_eq!(
        node.quote(while_live).settlement_amount(),
        Some(Amount::new(1_000_000_000, 9))
    );

    // One millisecond past its expiry there is no number at all — not the
    // number it was giving a moment ago.
    let after = Timestamp::from_millis(60_002);
    assert_eq!(
        node.quote(after),
        FeeQuote::Unsettleable {
            reason: UnsettleableReason::StaleOracleData
        }
    );
    assert_eq!(node.quote(after).settlement_amount(), None);

    // And the fee itself is untouched: it is unsettleable in OPEN, not
    // unpayable. The provider's declared USDC price still stands.
    assert_eq!(
        node.pricing().unwrap().amount,
        Amount::new(250_000, 6),
        "a dead feed changes nothing about what was declared"
    );
}

/// A node with no oracle records at all still participates: it cannot
/// quote, and it can still check what somebody else quoted.
#[test]
fn a_gossip_only_node_cannot_quote_but_can_still_verify() {
    let gateway = Keypair::from_seed([9; 32]);
    let payer = Keypair::generate();
    let quoting = Node::new(&gateway, 1, Some((4.0, 60_000)));
    let gossip_only = Node::new(&gateway, 4, None);

    assert_eq!(
        gossip_only.quote(NOW),
        FeeQuote::Unsettleable {
            reason: UnsettleableReason::NoOracleData
        },
        "no records means no price, and no invented one"
    );
    let signed = commit(quoting.quote(NOW), &payer);
    assert_eq!(
        gossip_only.verdict(&signed, Timestamp::from_millis(NOW.as_millis() + 1)),
        Ok(()),
        "checking arithmetic needs no oracle records, which is the point"
    );
}
