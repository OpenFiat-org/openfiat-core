//! What the node actually answers about a confidential trade channel.
//!
//! Written against the dispatch table rather than the registry, because
//! the registry was never the risk. The risk is a read handler that hands
//! a channel to whoever asks, or a `send` method that applies a payload
//! nobody in the trade signed — neither of which a unit test on
//! `TradeChannelRegistry` can catch.
//!
//! The decryption in these tests is deliberately done here, by the test,
//! with a key the test generated. Nothing in `openfiat-rpc` can do it:
//! there is no channel key anywhere in `NodeState` and no method that
//! takes one.

use openfiat_crypto::{Keypair, seal};
use openfiat_disputes::DisputeId;
use openfiat_disputes::events::{
    ArbitratorJoin, DisputeOpen, SignedArbitratorJoin, SignedDisputeOpen,
};
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_reservations::ReservationId;
use openfiat_rpc::dispatch::{MethodTable, encode_bytes, encode_peer_id};
use openfiat_rpc::methods::build_table;
use openfiat_rpc::state::NodeState;
use openfiat_settlement::SettlementId;
use openfiat_settlement::events::{SettlementInitiate, SignedSettlementInitiate};
use openfiat_storage::mem::MemoryStore;
use openfiat_tradechannel::events::{
    SignedTradeChannelEntryPost, SignedTradeChannelKeyGrant, TradeChannelEntryPost,
    TradeChannelKeyGrant,
};
use openfiat_tradechannel::{ChannelKey, EntryBinding, EntryKind, TradeChannel, seal_entry};
use openfiat_types::{Amount, PeerId, Timestamp};
use serde_json::Value;

const ACCOUNT_NUMBER: &[u8] = b"Equity Bank 0110123456789, R. Kimani";

fn peer(keypair: &Keypair) -> PeerId {
    peer_id_from_public_key(&keypair.public_key()).expect("a keypair derives a peer id")
}

fn settlement_id() -> SettlementId {
    SettlementId::new("s-channel-1")
}

/// A node holding one settlement between a buyer and a seller.
fn node_with_a_trade() -> (
    MethodTable<MemoryStore>,
    NodeState<MemoryStore>,
    Keypair,
    Keypair,
) {
    let state = NodeState::new_for_test(MemoryStore::new());
    let buyer = Keypair::from_seed([11u8; 32]);
    let seller = Keypair::from_seed([22u8; 32]);
    state
        .settlements
        .apply_initiate(SignedSettlementInitiate::sign(
            SettlementInitiate {
                id: settlement_id(),
                reservation_id: ReservationId::new("r-channel-1"),
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
    (build_table(), state, buyer, seller)
}

/// A wallet-proof params object for `getMyTradeChannel`, built the way a
/// real client would: ask for a nonce, sign it under this method's own
/// domain.
fn channel_proof(
    table: &MethodTable<MemoryStore>,
    state: &NodeState<MemoryStore>,
    subject: &PeerId,
    signer: &Keypair,
) -> Value {
    let challenge = table
        .dispatch(
            state,
            "getWalletChallenge",
            serde_json::json!({ "wallet": encode_peer_id(subject) }),
        )
        .expect("a nonce is free to ask for");
    let nonce = challenge["nonce"].as_str().expect("a nonce").to_string();
    let signing_bytes = openfiat_crypto::challenge::Challenge {
        subject: encode_peer_id(subject),
        nonce: nonce.clone(),
        expires_at: Timestamp::from_millis(challenge["expires_at"].as_u64().unwrap()),
    }
    .signing_bytes(openfiat_rpc::methods::settlement::CHANNEL_CHALLENGE_DOMAIN);

    serde_json::json!({
        "id": settlement_id().as_str(),
        "wallet": encode_peer_id(subject),
        "public_key": encode_bytes(signer.public_key().as_bytes()),
        "nonce": nonce,
        "signature": encode_bytes(
            &signer
                .sign(&signing_bytes)
                .as_bytes()
                .expect("a signature is 64 bytes"),
        ),
    })
}

/// The `data` parameter every `sendX` takes: base64 of the JSON-encoded,
/// already-signed payload.
fn send_params(payload: &impl serde::Serialize) -> Value {
    let bytes = openfiat_serialization::json::to_bytes(payload).expect("serializes");
    serde_json::json!({ "data": encode_bytes(&bytes) })
}

/// The seller opens a channel and discloses payment details through the
/// real RPC methods, returning the key only the test holds.
fn seller_discloses_payment_details(
    table: &MethodTable<MemoryStore>,
    state: &NodeState<MemoryStore>,
    buyer: &Keypair,
    seller: &Keypair,
) -> ChannelKey {
    let key = ChannelKey::generate();
    for recipient in [buyer, seller] {
        table
            .dispatch(
                state,
                "sendTradeChannelKeyGrant",
                send_params(&SignedTradeChannelKeyGrant::sign(
                    TradeChannelKeyGrant {
                        settlement_id: settlement_id(),
                        granter: peer(seller),
                        recipient: peer(recipient),
                        key_id: key.id(),
                        sealed_key: seal(&recipient.public_key(), key.expose()).unwrap(),
                        timestamp: Timestamp::from_millis(1_100),
                    },
                    seller,
                )),
            )
            .expect("a party may grant the key to a party");
    }

    let author = peer(seller);
    let payload = seal_entry(
        &key,
        &EntryBinding {
            settlement_id: &settlement_id(),
            author: &author,
            sequence: 0,
            kind: EntryKind::PaymentDetails.name(),
        },
        ACCOUNT_NUMBER,
    )
    .unwrap();
    table
        .dispatch(
            state,
            "sendTradeChannelEntry",
            send_params(&SignedTradeChannelEntryPost::sign(
                TradeChannelEntryPost {
                    settlement_id: settlement_id(),
                    author,
                    sequence: 0,
                    kind: EntryKind::PaymentDetails,
                    payload,
                    timestamp: Timestamp::from_millis(1_200),
                },
                seller,
            )),
        )
        .expect("a party may write to their own channel");
    key
}

fn channel_from(value: Value) -> TradeChannel {
    serde_json::from_value(value).expect("getMyTradeChannel returns a TradeChannel")
}

#[test]
fn a_party_reads_the_channel_and_decrypts_the_payment_details_with_their_own_key() {
    let (table, state, buyer, seller) = node_with_a_trade();
    seller_discloses_payment_details(&table, &state, &buyer, &seller);

    let channel = channel_from(
        table
            .dispatch(
                &state,
                "getMyTradeChannel",
                channel_proof(&table, &state, &peer(&buyer), &buyer),
            )
            .expect("a party reads their own channel"),
    );

    // The buyer never received a key from this node — they unseal the one
    // the seller addressed to them.
    let grant = channel.grants_for(&peer(&buyer))[0];
    let recovered = ChannelKey::from_bytes(
        openfiat_crypto::open(&buyer, &grant.sealed_key)
            .expect("the grant is sealed to the buyer")
            .try_into()
            .unwrap(),
    );
    let entry = channel.payment_details()[0];
    assert_eq!(
        openfiat_tradechannel::open_entry(&recovered, &entry.binding(), &entry.payload).unwrap(),
        ACCOUNT_NUMBER
    );
}

/// The adversarial case at the RPC boundary. The stranger proves a wallet
/// honestly — their own — and is refused rather than handed an empty
/// channel, for the reason `methods::wallet_auth` gives: a narrowing
/// implementation passes every test right up until a refactor drops the
/// narrowing.
#[test]
fn a_stranger_is_refused_the_channel_and_learns_nothing_from_the_refusal() {
    let (table, state, buyer, seller) = node_with_a_trade();
    seller_discloses_payment_details(&table, &state, &buyer, &seller);
    let stranger = Keypair::from_seed([99u8; 32]);

    let refused = table.dispatch(
        &state,
        "getMyTradeChannel",
        channel_proof(&table, &state, &peer(&stranger), &stranger),
    );
    assert!(
        refused.is_err(),
        "a peer who is neither a party nor a grant holder must be refused: {refused:?}"
    );
}

/// Even if the node did hand the ciphertext over — and every node on the
/// network holds it regardless — there is nothing in it.
#[test]
fn the_replicated_ciphertext_is_useless_to_a_third_party() {
    let (table, state, buyer, seller) = node_with_a_trade();
    seller_discloses_payment_details(&table, &state, &buyer, &seller);
    let stranger = Keypair::from_seed([99u8; 32]);

    let channel = state.trade_channels.channel(&settlement_id());
    for grant in &channel.grants {
        assert!(
            openfiat_crypto::open(&stranger, &grant.sealed_key).is_err(),
            "a grant addressed to a party must not open for anyone else"
        );
    }
    let entry = &channel.entries[0];
    assert!(
        !entry
            .payload
            .ciphertext
            .windows(ACCOUNT_NUMBER.len())
            .any(|window| window == ACCOUNT_NUMBER),
        "the account number must not be present in what the node stores"
    );
    assert!(
        openfiat_tradechannel::open_entry(
            &ChannelKey::generate(),
            &entry.binding(),
            &entry.payload
        )
        .is_err(),
        "without a grant there is no key, and without the key there is nothing"
    );
}

#[test]
fn a_stranger_cannot_write_into_someone_elses_channel_through_the_rpc() {
    let (table, state, _buyer, seller) = node_with_a_trade();
    let stranger = Keypair::from_seed([99u8; 32]);
    let key = ChannelKey::generate();
    let author = peer(&stranger);
    let payload = seal_entry(
        &key,
        &EntryBinding {
            settlement_id: &settlement_id(),
            author: &author,
            sequence: 0,
            kind: EntryKind::Message.name(),
        },
        b"pay to my account instead",
    )
    .unwrap();

    let refused = table.dispatch(
        &state,
        "sendTradeChannelEntry",
        send_params(&SignedTradeChannelEntryPost::sign(
            TradeChannelEntryPost {
                settlement_id: settlement_id(),
                author,
                sequence: 0,
                kind: EntryKind::Message,
                payload,
                timestamp: Timestamp::from_millis(1_300),
            },
            &stranger,
        )),
    );
    assert!(refused.is_err(), "{refused:?}");
    assert!(
        state
            .trade_channels
            .channel(&settlement_id())
            .entries
            .is_empty(),
        "a refused write must not have been applied first"
    );
    // And the seller's own channel is untouched, so a failed injection
    // cannot have displaced anything.
    seller_discloses_payment_details(&table, &state, &Keypair::from_seed([11u8; 32]), &seller);
    assert_eq!(
        state.trade_channels.channel(&settlement_id()).entries.len(),
        1
    );
}

/// The dispute path end to end: an arbitrator who is not a party, and
/// could not read the channel a moment ago, reads all of it once a party
/// grants them the key — and reads it through the same gated method,
/// which is what makes the grant the *only* thing that changed.
#[test]
fn an_arbitrator_reads_the_channel_only_after_a_party_grants_them_the_key() {
    let (table, state, buyer, seller) = node_with_a_trade();
    let key = seller_discloses_payment_details(&table, &state, &buyer, &seller);
    let arbitrator = Keypair::from_seed([44u8; 32]);

    state
        .disputes
        .apply_open(SignedDisputeOpen::sign(
            DisputeOpen {
                id: DisputeId::new("d-channel-1"),
                settlement_id: settlement_id(),
                opener: peer(&buyer),
                opener_public_key: buyer.public_key(),
                reason: "the account they gave me was closed".to_string(),
                timestamp: Timestamp::from_millis(1_400),
            },
            &buyer,
        ))
        .expect("a party may open a dispute");
    state
        .disputes
        .apply_arbitrator_join(SignedArbitratorJoin::sign(
            ArbitratorJoin {
                dispute_id: DisputeId::new("d-channel-1"),
                arbitrator: peer(&arbitrator),
                arbitrator_public_key: arbitrator.public_key(),
                timestamp: Timestamp::from_millis(1_500),
            },
            &arbitrator,
        ))
        .expect("an open case accepts arbitrators");

    // Joined, and still refused: arbitration does not by itself open a
    // conversation.
    let before = table.dispatch(
        &state,
        "getMyTradeChannel",
        channel_proof(&table, &state, &peer(&arbitrator), &arbitrator),
    );
    assert!(
        before.is_err(),
        "joining a dispute must not be enough on its own: {before:?}"
    );

    table
        .dispatch(
            &state,
            "sendTradeChannelKeyGrant",
            send_params(&SignedTradeChannelKeyGrant::sign(
                TradeChannelKeyGrant {
                    settlement_id: settlement_id(),
                    granter: peer(&buyer),
                    recipient: peer(&arbitrator),
                    key_id: key.id(),
                    sealed_key: seal(&arbitrator.public_key(), key.expose()).unwrap(),
                    timestamp: Timestamp::from_millis(1_600),
                },
                &buyer,
            )),
        )
        .expect("a party may disclose to a joined arbitrator");

    let channel = channel_from(
        table
            .dispatch(
                &state,
                "getMyTradeChannel",
                channel_proof(&table, &state, &peer(&arbitrator), &arbitrator),
            )
            .expect("a grant holder reads the channel"),
    );
    let grant = channel.grants_for(&peer(&arbitrator))[0];
    let recovered = ChannelKey::from_bytes(
        openfiat_crypto::open(&arbitrator, &grant.sealed_key)
            .expect("the grant is sealed to the arbitrator")
            .try_into()
            .unwrap(),
    );
    let entry = channel.payment_details()[0];
    assert_eq!(
        openfiat_tradechannel::open_entry(&recovered, &entry.binding(), &entry.payload).unwrap(),
        ACCOUNT_NUMBER,
        "the arbitrator reads the ciphertext the network carried before \
         the dispute existed, not something re-encrypted for them after it"
    );
}

/// Domain separation, asserted rather than assumed: the channel read has
/// its own separator, so a signature a wallet made to list its
/// settlements must not open its conversations.
#[test]
fn a_signature_made_for_the_settlements_surface_does_not_open_the_channel() {
    let (table, state, buyer, seller) = node_with_a_trade();
    seller_discloses_payment_details(&table, &state, &buyer, &seller);

    let challenge = table
        .dispatch(
            &state,
            "getWalletChallenge",
            serde_json::json!({ "wallet": encode_peer_id(&peer(&buyer)) }),
        )
        .unwrap();
    let nonce = challenge["nonce"].as_str().unwrap().to_string();
    let wrong_domain = openfiat_crypto::challenge::Challenge {
        subject: encode_peer_id(&peer(&buyer)),
        nonce: nonce.clone(),
        expires_at: Timestamp::from_millis(challenge["expires_at"].as_u64().unwrap()),
    }
    .signing_bytes(openfiat_rpc::methods::settlement::CHALLENGE_DOMAIN);

    let refused = table.dispatch(
        &state,
        "getMyTradeChannel",
        serde_json::json!({
            "id": settlement_id().as_str(),
            "wallet": encode_peer_id(&peer(&buyer)),
            "public_key": encode_bytes(buyer.public_key().as_bytes()),
            "nonce": nonce,
            "signature": encode_bytes(&buyer.sign(&wrong_domain).as_bytes().unwrap()),
        }),
    );
    assert!(
        refused.is_err(),
        "a cross-surface signature must not verify: {refused:?}"
    );
}
