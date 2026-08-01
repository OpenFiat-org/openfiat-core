//! The trade graph is not available to a stranger.
//!
//! `getCounterparties` refuses to say who a wallet trades with, on stated
//! physical-safety grounds. `getSettlements`, `getReservations` and
//! `getDisputes` used to answer the same question by returning every
//! record on the network with both parties named — so the gate was
//! bypassable by anyone who read the API listing.
//!
//! These tests are written against the dispatch table rather than the
//! redaction types, because the types were never the problem. A test on
//! `PublicSettlement::from` would have passed on the day the hole was
//! open; only asking the node what it actually answers can fail then.

use openfiat_advertisements::events::{AdvertisementCreate, SignedAdvertisementCreate};
use openfiat_advertisements::{AdvertisementId, Direction, PricingModel};
use openfiat_crypto::Keypair;
use openfiat_crypto::MintAddress;
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_reservations::ReservationId;
use openfiat_reservations::events::{ReservationRequest, SignedReservationRequest};
use openfiat_rpc::dispatch::{MethodTable, encode_bytes, encode_peer_id};
use openfiat_rpc::methods::build_table;
use openfiat_rpc::state::NodeState;
use openfiat_settlement::SettlementId;
use openfiat_settlement::events::{SettlementInitiate, SignedSettlementInitiate};
use openfiat_storage::mem::MemoryStore;
use openfiat_taxonomy::PaymentMethodRef;
use openfiat_types::{Amount, FiatCurrency, PeerId, Timestamp};
use serde_json::Value;

fn peer(keypair: &Keypair) -> PeerId {
    peer_id_from_public_key(&keypair.public_key()).expect("a keypair derives a peer id")
}

/// How a `PeerId` actually appears on the wire.
///
/// Not the base64 `encode_peer_id` spelling — that is what parameters use;
/// a serialized record carries the raw bytes as a JSON array. Asserting
/// against the wrong one is how a redaction test passes without testing
/// anything, which is exactly what happened to the first draft of these.
fn on_the_wire(keypair: &Keypair) -> String {
    serde_json::to_string(&peer(keypair)).expect("a peer id serializes")
}

/// One real settlement between two wallets, applied the way a gossiped
/// event would be.
fn network_with_a_trade() -> (
    MethodTable<MemoryStore>,
    NodeState<MemoryStore>,
    Keypair,
    Keypair,
) {
    let state = NodeState::new_for_test(MemoryStore::new());
    let buyer = Keypair::from_seed([11u8; 32]);
    let seller = Keypair::from_seed([22u8; 32]);
    let price = Amount::new(129_000_000, 6);

    // The advertisement and reservation are not decoration. `TradeView`
    // iterates reservations, so a fixture holding only a settlement
    // produces an empty `getTrades` — and a leak test against an empty
    // list passes without testing anything. This file has already passed
    // for the wrong reason twice; the assertion below is the guard.
    state
        .advertisements
        .apply_create(SignedAdvertisementCreate::sign(
            AdvertisementCreate {
                id: AdvertisementId::new("ad-public-1"),
                merchant: peer(&seller),
                merchant_public_key: seller.public_key(),
                asset_mint: MintAddress::parse("2bHPi5hA4zrmPAfrvLmEexg3KJjpTjNkUcxWnzUPeRRU")
                    .unwrap(),
                direction: Direction::Sell,
                fiat_currency: FiatCurrency::parse("KES").unwrap(),
                min_trade: Amount::new(1_000_000, 6),
                max_trade: Amount::new(10_000_000, 6),
                initial_liquidity: Amount::new(10_000_000, 6),
                pricing: PricingModel::Fixed { price },
                payment_methods: vec![PaymentMethodRef::builtin("mpesa-kenya").unwrap()],
                timestamp: Timestamp::from_millis(500),
            },
            &seller,
        ))
        .expect("a well-formed advertisement applies");

    state
        .reservations
        .apply_request(SignedReservationRequest::sign(
            ReservationRequest {
                id: ReservationId::new("r-public-1"),
                advertisement_id: AdvertisementId::new("ad-public-1"),
                requester: peer(&buyer),
                requester_public_key: buyer.public_key(),
                amount: Amount::new(2_500_000, 6),
                agreed_price: price,
                agreed_mid: None,
                timestamp: Timestamp::from_millis(900),
            },
            &buyer,
        ))
        .expect("a well-formed reservation applies");

    state
        .settlements
        .apply_initiate(SignedSettlementInitiate::sign(
            SettlementInitiate {
                id: SettlementId::new("s-public-1"),
                reservation_id: ReservationId::new("r-public-1"),
                buyer: peer(&buyer),
                buyer_public_key: buyer.public_key(),
                seller: peer(&seller),
                seller_public_key: seller.public_key(),
                amount: Amount::new(2_500_000, 6),
                timestamp: Timestamp::from_millis(1_000),
            },
            &buyer,
        ))
        .expect("a well-formed settlement applies");

    assert_eq!(
        state.trades.all().len(),
        1,
        "the fixture must actually produce a trade, or every assertion \
         below passes against an empty list"
    );
    (build_table(), state, buyer, seller)
}

#[test]
fn a_stranger_reading_every_settlement_learns_no_wallet() {
    let (table, state, buyer, seller) = network_with_a_trade();

    let answer = table
        .dispatch(&state, "getSettlements", serde_json::json!({}))
        .expect("the public read still answers");
    let json = answer.to_string();

    // The exact bytes a harvester would be looking for, in the form the
    // wire actually carries them.
    for wallet in [&buyer, &seller] {
        assert!(
            !json.contains(&on_the_wire(wallet)),
            "a party's wallet reached an unauthenticated caller: {json}"
        );
    }
    assert!(
        !json.contains("buyer") && !json.contains("seller"),
        "even the field names are gone, so nothing reads as an empty party: {json}"
    );

    // And it is still a useful public view, or removing the method would
    // have been the honest fix instead.
    assert!(json.contains("s-public-1") && json.contains("2500000"));
}

#[test]
fn the_by_id_read_cannot_be_used_to_walk_around_the_list() {
    // The bypass a partial fix invites: redact the enumerating method,
    // leave the singular one whole, and a caller iterates the ids the
    // first one hands out.
    let (table, state, buyer, _) = network_with_a_trade();

    let one = table
        .dispatch(
            &state,
            "getSettlement",
            serde_json::json!({ "id": "s-public-1" }),
        )
        .expect("a known id still resolves");
    assert!(
        !one.to_string().contains(&on_the_wire(&buyer)),
        "getSettlement must be redacted too: {one}"
    );
}

#[test]
fn a_party_reads_their_own_settlement_in_full() {
    let (table, state, buyer, seller) = network_with_a_trade();

    let challenge = table
        .dispatch(
            &state,
            "getWalletChallenge",
            serde_json::json!({ "wallet": encode_peer_id(&peer(&buyer)) }),
        )
        .expect("a nonce is free to ask for");
    let nonce = challenge["nonce"].as_str().expect("a nonce").to_string();

    // Signed under this surface's own domain. The same nonce signed for
    // `getCounterparties` would not verify here.
    let signing_bytes = openfiat_crypto::challenge::Challenge {
        subject: encode_peer_id(&peer(&buyer)),
        nonce: nonce.clone(),
        expires_at: Timestamp::from_millis(challenge["expires_at"].as_u64().unwrap()),
    }
    .signing_bytes(openfiat_rpc::methods::settlement::CHALLENGE_DOMAIN);

    let mine = table
        .dispatch(
            &state,
            "getMySettlements",
            serde_json::json!({
                "wallet": encode_peer_id(&peer(&buyer)),
                "public_key": encode_bytes(buyer.public_key().as_bytes()),
                "nonce": nonce,
                "signature": encode_bytes(&buyer.sign(&signing_bytes).as_bytes().expect("a signature is 64 bytes")),
            }),
        )
        .expect("a party proving their wallet reads their own trades");

    let json = mine.to_string();
    assert!(
        json.contains(&on_the_wire(&seller)),
        "a party already knows who they traded with — withholding it here \
         would protect nobody and break the trade room: {json}"
    );
    assert_eq!(mine.as_array().map(Vec::len), Some(1));
}

#[test]
fn a_stranger_cannot_read_someone_elses_settlements() {
    let (table, state, buyer, _) = network_with_a_trade();
    let stranger = Keypair::from_seed([99u8; 32]);

    let challenge = table
        .dispatch(
            &state,
            "getWalletChallenge",
            serde_json::json!({ "wallet": encode_peer_id(&peer(&buyer)) }),
        )
        .unwrap();
    let nonce = challenge["nonce"].as_str().unwrap().to_string();
    let signing_bytes = openfiat_crypto::challenge::Challenge {
        subject: encode_peer_id(&peer(&buyer)),
        nonce: nonce.clone(),
        expires_at: Timestamp::from_millis(challenge["expires_at"].as_u64().unwrap()),
    }
    .signing_bytes(openfiat_rpc::methods::settlement::CHALLENGE_DOMAIN);

    // The stranger signs honestly — with their own key — and asks about
    // somebody else's wallet. Refused, rather than quietly narrowed to
    // the stranger's own (empty) history: a filtering implementation
    // looks identical in every passing test until a refactor drops the
    // filter.
    let refused = table.dispatch(
        &state,
        "getMySettlements",
        serde_json::json!({
            "wallet": encode_peer_id(&peer(&buyer)),
            "public_key": encode_bytes(stranger.public_key().as_bytes()),
            "nonce": nonce,
            "signature": encode_bytes(&stranger.sign(&signing_bytes).as_bytes().expect("a signature is 64 bytes")),
        }),
    );
    assert!(
        refused.is_err(),
        "a key that does not derive to the wallet must be refused: {refused:?}"
    );
}

#[test]
fn a_signature_for_one_gated_surface_does_not_open_another() {
    // Domain separation, asserted rather than assumed. Both surfaces
    // draw nonces from the same ledger and identify their subject the
    // same way, so the only thing keeping them apart is what gets signed.
    let (table, state, buyer, _) = network_with_a_trade();

    let challenge = table
        .dispatch(
            &state,
            "getWalletChallenge",
            serde_json::json!({ "wallet": encode_peer_id(&peer(&buyer)) }),
        )
        .unwrap();
    let nonce = challenge["nonce"].as_str().unwrap().to_string();

    // Signed for the counterparties surface...
    let wrong_domain = openfiat_crypto::challenge::Challenge {
        subject: encode_peer_id(&peer(&buyer)),
        nonce: nonce.clone(),
        expires_at: Timestamp::from_millis(challenge["expires_at"].as_u64().unwrap()),
    }
    .signing_bytes(openfiat_rpc::methods::counterparties::CHALLENGE_DOMAIN);

    // ...and presented to the settlements one.
    let refused = table.dispatch(
        &state,
        "getMySettlements",
        serde_json::json!({
            "wallet": encode_peer_id(&peer(&buyer)),
            "public_key": encode_bytes(buyer.public_key().as_bytes()),
            "nonce": nonce,
            "signature": encode_bytes(&buyer.sign(&wrong_domain).as_bytes().expect("a signature is 64 bytes")),
        }),
    );
    assert!(
        refused.is_err(),
        "a cross-surface signature must not verify"
    );
}

#[test]
fn the_trade_join_is_not_a_way_around_the_redaction() {
    // `getTrades` embeds a reservation and its settlement whole, so
    // redacting the three underlying reads left the graph available one
    // method along. Two people found this independently by reading the
    // API listing, which is exactly who finds it.
    let (table, state, buyer, seller) = network_with_a_trade();

    for method in ["getTrades", "getTrade"] {
        let answer = table
            .dispatch(&state, method, serde_json::json!({ "id": "r-public-1" }))
            .unwrap_or_else(|err| panic!("{method} must still answer: {err:?}"));
        let json = answer.to_string();
        for wallet in [&buyer, &seller] {
            assert!(
                !json.contains(&on_the_wire(wallet)),
                "{method} leaked a party: {json}"
            );
        }
    }
}

#[test]
fn a_party_reads_their_own_trade_in_full() {
    let (table, state, buyer, seller) = network_with_a_trade();
    let mine = signed_read(
        &table,
        &state,
        &buyer,
        "getMyTrades",
        openfiat_rpc::methods::trade::CHALLENGE_DOMAIN,
    );
    assert!(
        mine.to_string().contains(&on_the_wire(&seller)),
        "a party to a trade already knows who they traded with: {mine}"
    );
}

/// One wallet-proof read, start to finish.
fn signed_read(
    table: &MethodTable<MemoryStore>,
    state: &NodeState<MemoryStore>,
    wallet: &Keypair,
    method: &str,
    domain: &str,
) -> Value {
    let challenge = table
        .dispatch(
            state,
            "getWalletChallenge",
            serde_json::json!({ "wallet": encode_peer_id(&peer(wallet)) }),
        )
        .unwrap();
    let nonce = challenge["nonce"].as_str().unwrap().to_string();
    let signing_bytes = openfiat_crypto::challenge::Challenge {
        subject: encode_peer_id(&peer(wallet)),
        nonce: nonce.clone(),
        expires_at: Timestamp::from_millis(challenge["expires_at"].as_u64().unwrap()),
    }
    .signing_bytes(domain);

    table
        .dispatch(
            state,
            method,
            serde_json::json!({
                "wallet": encode_peer_id(&peer(wallet)),
                "public_key": encode_bytes(wallet.public_key().as_bytes()),
                "nonce": nonce,
                "signature": encode_bytes(
                    &wallet.sign(&signing_bytes).as_bytes().expect("64 bytes")
                ),
            }),
        )
        .unwrap_or_else(|err| panic!("{method} refused a genuine proof: {err:?}"))
}
