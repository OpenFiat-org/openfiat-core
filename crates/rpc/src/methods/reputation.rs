//! Reputation methods (OFS-3000): the computed score, and the reviews
//! that sit beside it without ever being folded into it.
//!
//! `getReputation` returns the whole `ReputationProfile`, so every
//! dimension it gains — including §13's response counters and §14's
//! payment-discrepancy counters — appears here without a new method.
//! Rates are returned as their raw numerator and denominator rather than
//! a precomputed ratio, so a caller can aggregate across wallets and can
//! tell "no data yet" from "zero". There is no `sendReputationX`: see
//! `openfiat_reputation`'s crate doc for why a score has no signed event
//! type of its own.
//!
//! # Two answers to "is this wallet any good", kept apart
//!
//! A profile is evidence — every node recomputes it from the same signed
//! settlements and disputes. A review is an opinion, and its signature
//! proves who wrote it and nothing about whether it is true. They are
//! served by different methods, returning different types, from different
//! crates, and no review changes a single field of a profile: a score
//! that moved when somebody was rude about you would be a score two
//! colluding wallets could set to anything they liked. A client shows
//! both and lets a human weigh them, which is the only place that
//! judgement honestly belongs.
//!
//! # Why the public review read is not the party read
//!
//! A review names its author and, through its settlement, its subject —
//! which is an edge in exactly the trade graph `methods::redaction` and
//! `methods::counterparties` refuse to publish. `getReviews` is therefore
//! open but redacted (`PublicReview`: the subject, the stars, the words,
//! the day — never the author, never the settlement), and `getMyReviews`
//! returns the full records to a wallet that has proved it is one of the
//! two people in them. `openfiat_reviews::view` sets out the whole
//! argument, including why dropping the author alone would not have been
//! enough.

use crate::dispatch::{
    MethodTable, SendEventParams, WalletParams, decode_bytes, decode_peer_id, method_fn,
};
use crate::error::RpcError;
use crate::methods::wallet_auth::{WalletProof, verify_wallet};
use crate::state::NodeState;
use openfiat_reputation::ReputationProfile;
use openfiat_reviews::view::PublicReview;
use openfiat_reviews::{PublishedReview, ReviewError, SignedReviewPublish, protocol, subject_of};
use openfiat_serialization::{json, wire};
use openfiat_storage::KvStore;
use openfiat_types::Priority;

/// Domain separator for `getMyReviews`. A signature collected on another
/// gated surface can never be presented here, even though both draw their
/// nonces from the same ledger.
pub const CHALLENGE_DOMAIN: &str = "openfiat-my-reviews";

pub fn register<S: KvStore + 'static>(table: &mut MethodTable<S>) {
    table.register(
        "getReputation",
        method_fn(
            |state: &NodeState<S>, params: WalletParams| -> Result<ReputationProfile, RpcError> {
                Ok(state.reputation.profile(&decode_peer_id(&params.wallet)?))
            },
        ),
    );
    table.register(
        "getReviews",
        method_fn(
            |state: &NodeState<S>, params: WalletParams| -> Result<Vec<PublicReview>, RpcError> {
                // Open, like every other read of replicated state, and
                // redacted, unlike most of them. A wallet nobody has
                // reviewed gets an empty list rather than an error: never
                // having traded is not a failure, and an error here would
                // turn this into an oracle for whether a wallet exists.
                Ok(state
                    .reviews_view
                    .public_about(&decode_peer_id(&params.wallet)?))
            },
        ),
    );
    table.register(
        "getMyReviews",
        method_fn(
            |state: &NodeState<S>, params: WalletProof| -> Result<Vec<PublishedReview>, RpcError> {
                let wallet = verify_wallet(state, &params, CHALLENGE_DOMAIN)?;
                // Unredacted, and only for reviews this wallet wrote or is
                // the subject of. A party already knows who they traded
                // with; nothing is disclosed to them that they were not
                // present for. It is also how a client knows which of a
                // wallet's completed trades it has yet to review.
                Ok(state.reviews_view.involving(&wallet))
            },
        ),
    );
    table.register(
        "sendReviewPublish",
        method_fn(
            |state: &NodeState<S>, params: SendEventParams| -> Result<String, RpcError> {
                let bytes = decode_bytes(&params.data)?;
                let signed: SignedReviewPublish =
                    json::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;

                // The registry deliberately stores a review it cannot yet
                // authorize, because gossip may deliver one before the
                // trade it reviews (see `openfiat_reviews::store`). That
                // is right for a peer's record and wrong for the one being
                // submitted here: this caller is present, so a review they
                // were never entitled to write should come back as an
                // error rather than as a success that quietly shows up
                // nowhere. Checked only when this node actually holds the
                // settlement — an unknown one may simply not have
                // replicated yet, and refusing then would make a
                // legitimate review fail on a lagging node.
                if let Some(settlement) = state.settlements.get(&signed.review.settlement)
                    && subject_of(&settlement, &signed.review.author).is_none()
                {
                    return Err(RpcError::Application(ReviewError::NotAParty.code()));
                }

                let gossip_bytes =
                    wire::to_bytes(&signed).expect("SignedReviewPublish always serializes");
                let id = state
                    .reviews
                    .apply_publish(signed)
                    .map_err(|e| RpcError::Application(e.code()))?;
                crate::dispatch::originate(
                    state,
                    protocol::EVENT_PUBLISHED,
                    protocol::OFS_SPEC,
                    Priority::Reputation,
                    gossip_bytes,
                );
                Ok(id.as_str().to_string())
            },
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::{encode_bytes, encode_peer_id};
    use openfiat_crypto::Keypair;
    use openfiat_network::identity::peer_id_from_public_key;
    use openfiat_reservations::ReservationId;
    use openfiat_reviews::{Rating, Review};
    use openfiat_settlement::SettlementId;
    use openfiat_settlement::events::{
        PaymentSubmitted, SettlementApproved, SettlementInitiate, SignedPaymentSubmitted,
        SignedSettlementApproved, SignedSettlementInitiate,
    };
    use openfiat_storage::mem::MemoryStore;
    use openfiat_types::{Amount, ErrorCode, PeerId, Timestamp};

    fn table_and_state() -> (MethodTable<MemoryStore>, NodeState<MemoryStore>) {
        let mut table = MethodTable::new();
        register(&mut table);
        crate::methods::wallet_auth::register(&mut table);
        (table, NodeState::new_for_test(MemoryStore::new()))
    }

    fn peer(keypair: &Keypair) -> PeerId {
        peer_id_from_public_key(&keypair.public_key()).unwrap()
    }

    /// One approved settlement between the two, applied straight to the
    /// registry the way a gossiped event would.
    fn trade(state: &NodeState<MemoryStore>, id: &str, buyer: &Keypair, seller: &Keypair) {
        let settlement_id = SettlementId::new(id);
        let at = Timestamp::from_millis(1_000);
        state
            .settlements
            .apply_initiate(SignedSettlementInitiate::sign(
                SettlementInitiate {
                    id: settlement_id.clone(),
                    reservation_id: ReservationId::new(format!("res-{id}")),
                    buyer: peer(buyer),
                    buyer_public_key: buyer.public_key(),
                    seller: peer(seller),
                    seller_public_key: seller.public_key(),
                    amount: Amount::new(1_000_000, 6),
                    timestamp: at,
                },
                buyer,
            ))
            .unwrap();
        state
            .settlements
            .apply_payment_submitted(SignedPaymentSubmitted::sign(
                PaymentSubmitted {
                    settlement_id: settlement_id.clone(),
                    buyer: peer(buyer),
                    payment_reference: None,
                    timestamp: at,
                },
                buyer,
            ))
            .unwrap();
        state
            .settlements
            .apply_approved(SignedSettlementApproved::sign(
                SettlementApproved {
                    settlement_id,
                    seller: peer(seller),
                    timestamp: at,
                },
                seller,
            ))
            .unwrap();
    }

    fn publish(
        table: &MethodTable<MemoryStore>,
        state: &NodeState<MemoryStore>,
        settlement: &str,
        author: &Keypair,
        rating: Rating,
        comment: &str,
    ) -> Result<serde_json::Value, RpcError> {
        let review = Review {
            settlement: SettlementId::new(settlement),
            author: peer(author),
            author_public_key: author.public_key(),
            rating,
            comment: comment.to_string(),
            created_at: Timestamp::from_millis(2_000),
        };
        let signed = SignedReviewPublish::sign(review, author);
        table.dispatch(
            state,
            "sendReviewPublish",
            serde_json::json!({ "data": encode_bytes(&json::to_bytes(&signed).unwrap()) }),
        )
    }

    /// Deliberately returns the raw JSON rather than a typed struct: what
    /// this surface publishes is the serialized form, and a typed
    /// round-trip would hide a field being added to it.
    fn reviews_of(
        table: &MethodTable<MemoryStore>,
        state: &NodeState<MemoryStore>,
        wallet: &PeerId,
    ) -> Vec<serde_json::Value> {
        serde_json::from_value(
            table
                .dispatch(
                    state,
                    "getReviews",
                    serde_json::json!({ "wallet": encode_peer_id(wallet) }),
                )
                .expect("a public review read never fails"),
        )
        .unwrap()
    }

    fn my_reviews(
        table: &MethodTable<MemoryStore>,
        state: &NodeState<MemoryStore>,
        wallet: &Keypair,
    ) -> Result<Vec<serde_json::Value>, RpcError> {
        let challenge: openfiat_crypto::challenge::Challenge =
            serde_json::from_value(table.dispatch(
                state,
                "getWalletChallenge",
                serde_json::json!({ "wallet": encode_peer_id(&peer(wallet)) }),
            )?)
            .unwrap();
        let signature = wallet.sign(&challenge.signing_bytes(CHALLENGE_DOMAIN));
        let value = table.dispatch(
            state,
            "getMyReviews",
            serde_json::json!({
                "wallet": encode_peer_id(&peer(wallet)),
                "public_key": encode_bytes(wallet.public_key().as_bytes()),
                "nonce": challenge.nonce,
                "signature": encode_bytes(&signature.as_bytes().expect("64 bytes")),
            }),
        )?;
        Ok(serde_json::from_value(value).unwrap())
    }

    fn is(error: &RpcError, code: ErrorCode) -> bool {
        matches!(error, RpcError::Application(actual) if *actual == code)
    }

    #[test]
    fn a_party_reviews_their_counterparty_and_anyone_can_read_it() {
        let (table, state) = table_and_state();
        let buyer = Keypair::generate();
        let seller = Keypair::generate();
        trade(&state, "s-1", &buyer, &seller);

        publish(&table, &state, "s-1", &buyer, Rating::Five, "released fast")
            .expect("a party may review the trade they were in");

        let about_seller = reviews_of(&table, &state, &peer(&seller));
        assert_eq!(about_seller.len(), 1);
        assert_eq!(about_seller[0]["comment"], "released fast");
        assert_eq!(
            about_seller[0]["rating"], 5,
            "a rating crosses the wire as a number"
        );
    }

    /// The load-bearing test for this surface. Someone who was not a
    /// party holds a perfectly good key and signs correctly; what they
    /// lack is a trade. They must get an **error**, and their words must
    /// reach nobody.
    #[test]
    fn a_stranger_cannot_publish_a_review_of_a_settlement_they_were_not_in() {
        let (table, state) = table_and_state();
        let buyer = Keypair::generate();
        let seller = Keypair::generate();
        let stranger = Keypair::generate();
        trade(&state, "s-1", &buyer, &seller);

        let error = publish(&table, &state, "s-1", &stranger, Rating::One, "scammer")
            .expect_err("a wallet that was not in the trade must be refused");
        assert!(
            is(&error, ErrorCode::InvalidIdentityClaim),
            "the refusal must name the false claim, not look like a success: {error:?}"
        );

        for wallet in [&peer(&buyer), &peer(&seller), &peer(&stranger)] {
            assert!(
                reviews_of(&table, &state, wallet).is_empty(),
                "a non-party's opinion is about nobody and must surface nowhere"
            );
        }
    }

    /// The same refusal from the other direction: a trade that never
    /// settled cannot be reviewed by anyone, including a real party.
    #[test]
    fn a_trade_still_in_flight_cannot_be_reviewed_by_its_own_parties() {
        let (table, state) = table_and_state();
        let buyer = Keypair::generate();
        let seller = Keypair::generate();
        let at = Timestamp::from_millis(1_000);
        state
            .settlements
            .apply_initiate(SignedSettlementInitiate::sign(
                SettlementInitiate {
                    id: SettlementId::new("s-1"),
                    reservation_id: ReservationId::new("res-1"),
                    buyer: peer(&buyer),
                    buyer_public_key: buyer.public_key(),
                    seller: peer(&seller),
                    seller_public_key: seller.public_key(),
                    amount: Amount::new(1_000_000, 6),
                    timestamp: at,
                },
                &buyer,
            ))
            .unwrap();

        let error = publish(&table, &state, "s-1", &buyer, Rating::One, "too slow")
            .expect_err("nothing has happened yet to review");
        assert!(is(&error, ErrorCode::InvalidIdentityClaim), "{error:?}");
    }

    /// One trade is one review. A party may amend their own words — every
    /// node resolves which version stands identically, see
    /// `ReviewRegistry::apply_publish` — but they cannot bury a
    /// counterparty under a pile of rows, which is what an unlimited
    /// review-per-trade count would be worth to an abuser.
    #[test]
    fn a_second_review_of_one_trade_replaces_the_first_rather_than_stacking() {
        let (table, state) = table_and_state();
        let buyer = Keypair::generate();
        let seller = Keypair::generate();
        trade(&state, "s-1", &buyer, &seller);

        for comment in ["great", "on reflection, fine", "actually great"] {
            let _ = publish(&table, &state, "s-1", &buyer, Rating::Five, comment);
        }
        assert_eq!(
            reviews_of(&table, &state, &peer(&seller)).len(),
            1,
            "three submissions, one review"
        );
    }

    #[test]
    fn resubmitting_a_review_already_on_file_is_refused_rather_than_duplicated() {
        let (table, state) = table_and_state();
        let buyer = Keypair::generate();
        let seller = Keypair::generate();
        trade(&state, "s-1", &buyer, &seller);

        publish(&table, &state, "s-1", &buyer, Rating::Five, "great").unwrap();
        let error = publish(&table, &state, "s-1", &buyer, Rating::Five, "great")
            .expect_err("the identical record is already the one that stands");
        assert!(is(&error, ErrorCode::ResourceAlreadyExists), "{error:?}");
    }

    /// The privacy answer, asserted on what actually crosses the wire.
    #[test]
    fn the_public_read_gives_away_neither_the_author_nor_the_trade() {
        let (table, state) = table_and_state();
        let buyer = Keypair::generate();
        let seller = Keypair::generate();
        trade(&state, "s-1", &buyer, &seller);
        publish(&table, &state, "s-1", &buyer, Rating::Five, "released fast").unwrap();
        publish(&table, &state, "s-1", &seller, Rating::Two, "paid late").unwrap();

        let both = serde_json::to_string(&[
            table
                .dispatch(
                    &state,
                    "getReviews",
                    serde_json::json!({ "wallet": encode_peer_id(&peer(&buyer)) }),
                )
                .unwrap(),
            table
                .dispatch(
                    &state,
                    "getReviews",
                    serde_json::json!({ "wallet": encode_peer_id(&peer(&seller)) }),
                )
                .unwrap(),
        ])
        .unwrap();

        assert!(both.contains("released fast") && both.contains("paid late"));
        for leaked in ["s-1", "author", "settlement"] {
            assert!(
                !both.contains(leaked),
                "{leaked:?} rejoins the two parties this network keeps apart: {both}"
            );
        }
    }

    #[test]
    fn a_party_reads_the_full_record_of_reviews_they_are_in() {
        let (table, state) = table_and_state();
        let buyer = Keypair::generate();
        let seller = Keypair::generate();
        trade(&state, "s-1", &buyer, &seller);
        publish(&table, &state, "s-1", &buyer, Rating::Five, "released fast").unwrap();

        let mine = my_reviews(&table, &state, &buyer).expect("my own key reads my own reviews");
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0]["settlement"], "s-1", "a party may see which trade");
        assert_eq!(mine[0]["author"], serde_json::json!(peer(&buyer)));
    }

    /// The wallet-proof gate is only worth having if it refuses rather
    /// than narrows — a filtering implementation looks identical in every
    /// passing test right up until a refactor drops the filter.
    #[test]
    fn asking_for_another_wallets_reviews_is_refused_not_filtered() {
        let (table, state) = table_and_state();
        let buyer = Keypair::generate();
        let seller = Keypair::generate();
        trade(&state, "s-1", &buyer, &seller);
        publish(&table, &state, "s-1", &buyer, Rating::Five, "released fast").unwrap();

        let attacker = Keypair::generate();
        let challenge: openfiat_crypto::challenge::Challenge = serde_json::from_value(
            table
                .dispatch(
                    &state,
                    "getWalletChallenge",
                    serde_json::json!({ "wallet": encode_peer_id(&peer(&buyer)) }),
                )
                .unwrap(),
        )
        .unwrap();
        let signature = attacker.sign(&challenge.signing_bytes(CHALLENGE_DOMAIN));
        let error = table
            .dispatch(
                &state,
                "getMyReviews",
                serde_json::json!({
                    "wallet": encode_peer_id(&peer(&buyer)),
                    "public_key": encode_bytes(attacker.public_key().as_bytes()),
                    "nonce": challenge.nonce,
                    "signature": encode_bytes(&signature.as_bytes().unwrap()),
                }),
            )
            .expect_err("a wallet you cannot prove you control must be refused");
        assert!(is(&error, ErrorCode::InvalidIdentityClaim), "{error:?}");
    }

    #[test]
    fn a_wallet_nobody_has_reviewed_gets_an_empty_list_rather_than_an_error() {
        let (table, state) = table_and_state();
        assert!(reviews_of(&table, &state, &peer(&Keypair::generate())).is_empty());
    }
}
