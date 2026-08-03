//! Proves the four ways *out* of a trade are actually reachable.
//!
//! `ReservationRegistry::apply_cancel`, `SettlementRegistry::apply_rejected`,
//! `::apply_cancelled` and `::apply_payment_reversed` were each complete,
//! unit tested, and named by a gossip constant, and not one of them had an
//! RPC method. They were the ninth and tenth times this workspace shipped
//! correct code nothing calls, and every previous one had passing unit
//! tests for the function itself — a test that calls `apply_cancel` cannot
//! tell you whether anyone else does.
//!
//! The four are one subject, not four: between them they are every way a
//! trade ends other than completing, and each is the answer to "what does
//! a user do when this goes wrong". Missing any one of them pushes a real
//! situation into the dispute system, which is why they are guarded
//! together.
//!
//! So these tests never call an `apply_*` to make their point. They ask
//! the dispatch table the server actually builds, by the exact method
//! names a client sends, and then check the record moved. A registration
//! that exists but is wired to the wrong registry call, or to nothing,
//! fails here; a name quietly renamed fails here; a whole `table.register`
//! block deleted fails here.
//!
//! # Why this is not the `include_str!` pattern
//!
//! `crates/reservations/tests/node_sweep_wiring.rs` reads
//! `crates/rpc/src/actor.rs` as *text*, because `openfiat-reservations`
//! cannot depend on `openfiat-rpc` — that would be a cycle — and text was
//! the only way to assert on a caller it cannot link against. There is no
//! cycle here: this file lives in `openfiat-rpc`'s own test target and
//! links the real `build_table`. Asserting against the table itself is
//! strictly stronger than matching source text, so it is what this does.
//! Text matching stays the exception it was introduced as.

use openfiat_advertisements::events::{AdvertisementCreate, SignedAdvertisementCreate};
use openfiat_advertisements::{AdvertisementId, Direction, PricingModel};
use openfiat_crypto::{Keypair, MintAddress};
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_reservations::events::{
    ReservationCancel, ReservationRequest, SignedReservationCancel, SignedReservationRequest,
};
use openfiat_reservations::{ReservationId, ReservationState};
use openfiat_rpc::dispatch::{MethodTable, encode_bytes};
use openfiat_rpc::methods::build_table;
use openfiat_rpc::state::NodeState;
use openfiat_settlement::events::{
    PaymentReversed, PaymentSubmitted, SettlementApproved, SettlementCancelled, SettlementInitiate,
    SettlementRejected, SignedPaymentReversed, SignedPaymentSubmitted, SignedSettlementApproved,
    SignedSettlementCancelled, SignedSettlementInitiate, SignedSettlementRejected,
};
use openfiat_settlement::record::PaymentDiscrepancy;
use openfiat_settlement::{SettlementId, SettlementState};
use openfiat_storage::mem::MemoryStore;
use openfiat_taxonomy::PaymentMethodRef;
use openfiat_types::{Amount, FiatCurrency, PeerId, Timestamp};
use serde_json::Value;

/// The four method names a client sends. Written out here, once, so the
/// failure messages below can name the thing that is missing rather than
/// describing it.
const RESERVATION_CANCEL: &str = "sendReservationCancel";
const SETTLEMENT_REJECTED: &str = "sendSettlementRejected";
const SETTLEMENT_CANCELLED: &str = "sendSettlementCancelled";
const PAYMENT_REVERSED: &str = "sendPaymentReversed";

const AD: &str = "ad-exit-1";
const RESERVATION: &str = "res-exit-1";
const SETTLEMENT: &str = "settle-exit-1";

fn peer(keypair: &Keypair) -> PeerId {
    peer_id_from_public_key(&keypair.public_key()).expect("a keypair derives a peer id")
}

fn price() -> Amount {
    Amount::new(129_000_000, 6)
}

fn size() -> Amount {
    Amount::new(2_500_000, 6)
}

/// A `sendX` payload the way the wire carries it: canonical JSON of the
/// signed event, base64'd into `{ "data": ... }`.
fn send_params<T: serde::Serialize>(payload: &T) -> Value {
    let bytes = openfiat_serialization::json::to_bytes(payload).expect("a signed event serializes");
    serde_json::json!({ "data": encode_bytes(&bytes) })
}

/// One merchant advertisement with a live reservation against it.
fn a_reservation() -> (
    MethodTable<MemoryStore>,
    NodeState<MemoryStore>,
    Keypair,
    Keypair,
) {
    let state = NodeState::new_for_test(MemoryStore::new());
    let buyer = Keypair::from_seed([31u8; 32]);
    let seller = Keypair::from_seed([32u8; 32]);

    state
        .advertisements
        .apply_create(SignedAdvertisementCreate::sign(
            AdvertisementCreate {
                id: AdvertisementId::new(AD),
                merchant: peer(&seller),
                merchant_public_key: seller.public_key(),
                asset_mint: MintAddress::parse("2bHPi5hA4zrmPAfrvLmEexg3KJjpTjNkUcxWnzUPeRRU")
                    .expect("a valid mint"),
                direction: Direction::Sell,
                fiat_currency: FiatCurrency::parse("KES").expect("a valid currency"),
                min_trade: Amount::new(1_000_000, 6),
                max_trade: Amount::new(10_000_000, 6),
                initial_liquidity: Amount::new(10_000_000, 6),
                pricing: PricingModel::Fixed { price: price() },
                payment_methods: vec![PaymentMethodRef::builtin("mpesa-kenya").expect("a rail")],
                timestamp: Timestamp::from_millis(500),
            },
            &seller,
        ))
        .expect("a well-formed advertisement applies");

    state
        .reservations
        .apply_request(SignedReservationRequest::sign(
            ReservationRequest {
                id: ReservationId::new(RESERVATION),
                advertisement_id: AdvertisementId::new(AD),
                requester: peer(&buyer),
                requester_public_key: buyer.public_key(),
                amount: size(),
                agreed_price: price(),
                agreed_mid: None,
                timestamp: Timestamp::now(),
            },
            &buyer,
        ))
        .expect("a well-formed reservation applies");

    (build_table(), state, buyer, seller)
}

/// The same, plus a settlement raised against that reservation.
fn a_settlement() -> (
    MethodTable<MemoryStore>,
    NodeState<MemoryStore>,
    Keypair,
    Keypair,
) {
    let (table, state, buyer, seller) = a_reservation();
    state
        .settlements
        .apply_initiate(SignedSettlementInitiate::sign(
            SettlementInitiate {
                id: SettlementId::new(SETTLEMENT),
                reservation_id: ReservationId::new(RESERVATION),
                buyer: peer(&buyer),
                buyer_public_key: buyer.public_key(),
                seller: peer(&seller),
                seller_public_key: seller.public_key(),
                amount: size(),
                timestamp: Timestamp::from_millis(1_000),
            },
            &buyer,
        ))
        .expect("a well-formed settlement applies");
    (table, state, buyer, seller)
}

fn declare_payment(
    table: &MethodTable<MemoryStore>,
    state: &NodeState<MemoryStore>,
    buyer: &Keypair,
) {
    table
        .dispatch(
            state,
            "sendPaymentSubmitted",
            send_params(&SignedPaymentSubmitted::sign(
                PaymentSubmitted {
                    settlement_id: SettlementId::new(SETTLEMENT),
                    buyer: peer(buyer),
                    payment_reference: Some("TXN-EXIT".to_string()),
                    timestamp: Timestamp::from_millis(1_100),
                },
                buyer,
            )),
        )
        .expect("the buyer declares payment");
}

/// The whole point of the file, stated once as a list: these four names
/// must exist on the surface a client talks to.
///
/// Deliberately separate from the behavioural tests below. A registration
/// removed makes several of those fail, but only this one says *what* is
/// gone without the reader having to infer it from a state assertion.
#[test]
fn every_exit_from_a_trade_is_registered_on_the_rpc_surface() {
    let table: MethodTable<MemoryStore> = build_table();
    let names = table.method_names();
    for method in [
        RESERVATION_CANCEL,
        SETTLEMENT_REJECTED,
        SETTLEMENT_CANCELLED,
        PAYMENT_REVERSED,
    ] {
        assert!(
            names.contains(&method),
            "`{method}` is no longer registered in openfiat-rpc's dispatch table. The registry \
             call behind it still exists and is still unit tested, which is exactly why this \
             test is here: ten mechanisms in this workspace have shipped complete, tested and \
             unreachable, and each one passed its own tests the whole time. These four are every \
             way a trade ends other than completing. Without them a taker cannot abandon a \
             reservation without holding the merchant's liquidity for the full 30-minute \
             VALIDATION_WINDOW, a merchant cannot refuse a payment without opening a dispute — a \
             filing fee, arbitrators and a frozen escrow to say no — and a buyer who declared \
             payment by mistake cannot take it back at all. Restore the `table.register` block \
             in crates/rpc/src/methods/{{reservations, settlement}}.rs. If you are reading this \
             before that block first landed, the failure is the report, not a broken test."
        );
    }
}

/// Registered *and* wired to `apply_cancel`: the reservation moves and the
/// merchant's liquidity comes back. A registration pointing at the wrong
/// call, or at nothing, passes the test above and fails this one.
#[test]
fn a_taker_cancels_their_reservation_and_the_merchant_gets_the_liquidity_back() {
    let (table, state, buyer, _seller) = a_reservation();
    let ad = AdvertisementId::new(AD);
    let locked = state
        .advertisements
        .get(&ad)
        .expect("the advertisement exists")
        .available_liquidity;
    assert_eq!(
        locked,
        Amount::new(7_500_000, 6),
        "the fixture must actually have liquidity locked, or the assertion below proves nothing"
    );

    table
        .dispatch(
            &state,
            RESERVATION_CANCEL,
            send_params(&SignedReservationCancel::sign(
                ReservationCancel {
                    id: ReservationId::new(RESERVATION),
                    requester: peer(&buyer),
                    timestamp: Timestamp::now(),
                },
                &buyer,
            )),
        )
        .expect("the requester may cancel their own reservation");

    assert_eq!(
        state
            .reservations
            .get(&ReservationId::new(RESERVATION))
            .expect("the reservation survives cancellation as a record")
            .state,
        ReservationState::Cancelled
    );
    assert_eq!(
        state
            .advertisements
            .get(&ad)
            .expect("the advertisement exists")
            .available_liquidity,
        Amount::new(10_000_000, 6),
        "cancelling must return the reserved liquidity immediately rather than leaving the \
         merchant waiting for the expiry sweep"
    );
}

/// Exposing a method is the moment its authorization starts mattering:
/// before this wiring `apply_cancel`'s owner check was only reachable by a
/// node operator crafting a gossip event, and now anyone can POST at it.
#[test]
fn a_stranger_cannot_cancel_someone_elses_reservation_through_the_rpc_surface() {
    let (table, state, buyer, _seller) = a_reservation();
    let thief = Keypair::from_seed([99u8; 32]);

    // Two shapes, because they fail on different checks and a fix that
    // caught only one would look like it worked.
    let signed_by_a_stranger = SignedReservationCancel::sign(
        ReservationCancel {
            // The real owner named, so only the signature is wrong.
            id: ReservationId::new(RESERVATION),
            requester: peer(&buyer),
            timestamp: Timestamp::now(),
        },
        &thief,
    );
    let claimed_by_a_stranger = SignedReservationCancel::sign(
        ReservationCancel {
            id: ReservationId::new(RESERVATION),
            requester: peer(&thief),
            timestamp: Timestamp::now(),
        },
        &thief,
    );

    for attempt in [signed_by_a_stranger, claimed_by_a_stranger] {
        assert!(
            table
                .dispatch(&state, RESERVATION_CANCEL, send_params(&attempt))
                .is_err(),
            "a wallet that does not own a reservation must not be able to cancel it"
        );
    }
    assert_eq!(
        state
            .reservations
            .get(&ReservationId::new(RESERVATION))
            .expect("the reservation exists")
            .state,
        ReservationState::EscrowLocked,
        "a refused cancel must leave the reservation exactly where it was"
    );
}

/// The merchant's "no", without a dispute. `discrepancy` is recorded
/// because it is what reputation counts — a rejection that stored only the
/// free-text reason would be unreadable to everything but a human.
#[test]
fn a_merchant_rejects_a_payment_without_opening_a_dispute() {
    let (table, state, buyer, seller) = a_settlement();
    declare_payment(&table, &state, &buyer);

    table
        .dispatch(
            &state,
            SETTLEMENT_REJECTED,
            send_params(&SignedSettlementRejected::sign(
                SettlementRejected {
                    settlement_id: SettlementId::new(SETTLEMENT),
                    seller: peer(&seller),
                    reason: "no matching deposit".to_string(),
                    discrepancy: PaymentDiscrepancy::IncorrectAmount,
                    timestamp: Timestamp::from_millis(1_200),
                },
                &seller,
            )),
        )
        .expect("the seller may refuse a payment they cannot find");

    let settlement = state
        .settlements
        .get(&SettlementId::new(SETTLEMENT))
        .expect("the settlement exists");
    assert_eq!(settlement.state, SettlementState::Rejected);
    assert_eq!(
        settlement.payment_discrepancy,
        Some(PaymentDiscrepancy::IncorrectAmount),
        "the machine-readable half of a rejection is what reputation reads; losing it would \
         leave only prose"
    );
    assert_eq!(
        state.disputes.all().len(),
        0,
        "the whole point of this method is that refusing a payment costs no dispute"
    );
}

/// A rejection is the merchant's claim, not an adjudication. A buyer who
/// really did pay must still be able to escalate afterwards, or this
/// method would be a way for a merchant to end a trade unilaterally.
#[test]
fn a_rejected_settlement_can_still_be_disputed() {
    let (table, state, buyer, seller) = a_settlement();
    declare_payment(&table, &state, &buyer);
    table
        .dispatch(
            &state,
            SETTLEMENT_REJECTED,
            send_params(&SignedSettlementRejected::sign(
                SettlementRejected {
                    settlement_id: SettlementId::new(SETTLEMENT),
                    seller: peer(&seller),
                    reason: "nothing arrived".to_string(),
                    discrepancy: PaymentDiscrepancy::Other,
                    timestamp: Timestamp::from_millis(1_200),
                },
                &seller,
            )),
        )
        .expect("the seller rejects");

    let opened = table.dispatch(
        &state,
        "sendDisputeOpen",
        send_params(&openfiat_disputes::events::SignedDisputeOpen::sign(
            openfiat_disputes::events::DisputeOpen {
                id: openfiat_disputes::DisputeId::new("dispute-exit-1"),
                settlement_id: SettlementId::new(SETTLEMENT),
                opener: peer(&buyer),
                opener_public_key: buyer.public_key(),
                reason: "I have the bank receipt".to_string(),
                timestamp: Timestamp::from_millis(1_300),
            },
            &buyer,
        )),
    );
    assert!(
        opened.is_ok(),
        "rejection must not be a dead end for a buyer who really paid: {opened:?}"
    );
}

/// A rejection before the buyer has declared anything would let a merchant
/// close a settlement the moment it opened.
#[test]
fn a_payment_cannot_be_rejected_before_it_has_been_declared() {
    let (table, state, _buyer, seller) = a_settlement();

    let refused = table.dispatch(
        &state,
        SETTLEMENT_REJECTED,
        send_params(&SignedSettlementRejected::sign(
            SettlementRejected {
                settlement_id: SettlementId::new(SETTLEMENT),
                seller: peer(&seller),
                reason: "pre-emptive".to_string(),
                discrepancy: PaymentDiscrepancy::Other,
                timestamp: Timestamp::from_millis(1_050),
            },
            &seller,
        )),
    );
    assert!(
        refused.is_err(),
        "a settlement still in AwaitingPayment has no payment to reject"
    );
}

#[test]
fn either_party_may_cancel_a_settlement_before_payment_is_declared() {
    for canceller_is_the_seller in [false, true] {
        let (table, state, buyer, seller) = a_settlement();
        let canceller = if canceller_is_the_seller {
            &seller
        } else {
            &buyer
        };

        table
            .dispatch(
                &state,
                SETTLEMENT_CANCELLED,
                send_params(&SignedSettlementCancelled::sign(
                    SettlementCancelled {
                        settlement_id: SettlementId::new(SETTLEMENT),
                        canceller: peer(canceller),
                        timestamp: Timestamp::from_millis(1_100),
                    },
                    canceller,
                )),
            )
            .expect("either party may walk away before payment is declared");

        assert_eq!(
            state
                .settlements
                .get(&SettlementId::new(SETTLEMENT))
                .expect("the settlement exists")
                .state,
            SettlementState::Cancelled
        );
    }
}

/// The restriction that stops this being a theft primitive: once the buyer
/// says the money is sent, a merchant cannot make the settlement vanish.
#[test]
fn a_settlement_cannot_be_cancelled_once_payment_has_been_declared() {
    let (table, state, buyer, seller) = a_settlement();
    declare_payment(&table, &state, &buyer);

    let refused = table.dispatch(
        &state,
        SETTLEMENT_CANCELLED,
        send_params(&SignedSettlementCancelled::sign(
            SettlementCancelled {
                settlement_id: SettlementId::new(SETTLEMENT),
                canceller: peer(&seller),
                timestamp: Timestamp::from_millis(1_200),
            },
            &seller,
        )),
    );
    assert!(
        refused.is_err(),
        "a merchant must not be able to cancel a settlement out from under a declared payment"
    );
    assert_eq!(
        state
            .settlements
            .get(&SettlementId::new(SETTLEMENT))
            .expect("the settlement exists")
            .state,
        SettlementState::PaymentSubmitted
    );
}

#[test]
fn a_stranger_cannot_cancel_a_settlement_they_are_not_party_to() {
    let (table, state, _buyer, _seller) = a_settlement();
    let stranger = Keypair::from_seed([98u8; 32]);

    let refused = table.dispatch(
        &state,
        SETTLEMENT_CANCELLED,
        send_params(&SignedSettlementCancelled::sign(
            SettlementCancelled {
                settlement_id: SettlementId::new(SETTLEMENT),
                canceller: peer(&stranger),
                timestamp: Timestamp::from_millis(1_100),
            },
            &stranger,
        )),
    );
    assert!(
        refused.is_err(),
        "only the buyer or the seller may cancel their settlement"
    );
    assert_eq!(
        state
            .settlements
            .get(&SettlementId::new(SETTLEMENT))
            .expect("the settlement exists")
            .state,
        SettlementState::AwaitingPayment
    );
}

/// The buyer's way back from a mis-click.
///
/// Both cleared fields are asserted, and neither is bookkeeping.
/// `payment_reference` is the bank reference the merchant would go looking
/// for; leaving it behind would have a withdrawn declaration still
/// pointing at a transfer. `payment_submitted_at` is what
/// `openfiat_reputation` reads as *both* "this buyer made a payment" and
/// "this merchant is on the clock to answer one", so a reversal that left
/// it set would credit the buyer with a payment they took back and keep
/// faulting the merchant for not answering it.
#[test]
fn a_buyer_takes_back_a_payment_declaration_they_made_by_mistake() {
    let (table, state, buyer, _seller) = a_settlement();
    declare_payment(&table, &state, &buyer);
    let declared = state
        .settlements
        .get(&SettlementId::new(SETTLEMENT))
        .expect("the settlement exists");
    assert_eq!(
        declared.payment_reference.as_deref(),
        Some("TXN-EXIT"),
        "the fixture must actually have a declaration to withdraw, or the assertions below \
         pass against nothing"
    );
    assert!(declared.payment_submitted_at.is_some());

    table
        .dispatch(
            &state,
            PAYMENT_REVERSED,
            send_params(&SignedPaymentReversed::sign(
                PaymentReversed {
                    settlement_id: SettlementId::new(SETTLEMENT),
                    buyer: peer(&buyer),
                    timestamp: Timestamp::from_millis(1_200),
                },
                &buyer,
            )),
        )
        .expect("the buyer may withdraw their own declaration");

    let withdrawn = state
        .settlements
        .get(&SettlementId::new(SETTLEMENT))
        .expect("the settlement exists");
    assert_eq!(withdrawn.state, SettlementState::AwaitingPayment);
    assert_eq!(
        withdrawn.payment_reference, None,
        "a withdrawn declaration must not leave the merchant a reference to hunt for"
    );
    assert_eq!(
        withdrawn.payment_submitted_at, None,
        "openfiat_reputation reads this field as both the buyer's payment and the merchant's \
         outstanding response; a reversal that left it set would count both"
    );
}

/// Only the buyer declares payment, so only the buyer may withdraw it. A
/// merchant able to reverse would be able to erase a declaration they are
/// on the clock to answer.
#[test]
fn only_the_buyer_may_withdraw_their_own_payment_declaration() {
    let (table, state, buyer, seller) = a_settlement();
    declare_payment(&table, &state, &buyer);

    for (who, signer) in [
        ("the seller", &seller),
        ("a stranger", &Keypair::from_seed([97u8; 32])),
    ] {
        let refused = table.dispatch(
            &state,
            PAYMENT_REVERSED,
            send_params(&SignedPaymentReversed::sign(
                PaymentReversed {
                    settlement_id: SettlementId::new(SETTLEMENT),
                    buyer: peer(signer),
                    timestamp: Timestamp::from_millis(1_200),
                },
                signer,
            )),
        );
        assert!(
            refused.is_err(),
            "{who} must not be able to withdraw the buyer's declaration"
        );
    }
    assert_eq!(
        state
            .settlements
            .get(&SettlementId::new(SETTLEMENT))
            .expect("the settlement exists")
            .state,
        SettlementState::PaymentSubmitted
    );
}

/// Reversal must not be a way out of a decision already taken. Approval
/// and rejection both leave `PaymentSubmitted`, which is the only state
/// reversal is legal from — so this is really asserting that the state
/// check is the thing enforcing it, from both directions.
#[test]
fn a_declaration_cannot_be_withdrawn_once_the_merchant_has_answered_it() {
    for merchant_approves in [true, false] {
        let (table, state, buyer, seller) = a_settlement();
        declare_payment(&table, &state, &buyer);

        let expected = if merchant_approves {
            table
                .dispatch(
                    &state,
                    "sendSettlementApproved",
                    send_params(&SignedSettlementApproved::sign(
                        SettlementApproved {
                            settlement_id: SettlementId::new(SETTLEMENT),
                            seller: peer(&seller),
                            timestamp: Timestamp::from_millis(1_200),
                        },
                        &seller,
                    )),
                )
                .expect("the seller approves");
            SettlementState::Approved
        } else {
            table
                .dispatch(
                    &state,
                    SETTLEMENT_REJECTED,
                    send_params(&SignedSettlementRejected::sign(
                        SettlementRejected {
                            settlement_id: SettlementId::new(SETTLEMENT),
                            seller: peer(&seller),
                            reason: "nothing arrived".to_string(),
                            discrepancy: PaymentDiscrepancy::Other,
                            timestamp: Timestamp::from_millis(1_200),
                        },
                        &seller,
                    )),
                )
                .expect("the seller rejects");
            SettlementState::Rejected
        };

        let refused = table.dispatch(
            &state,
            PAYMENT_REVERSED,
            send_params(&SignedPaymentReversed::sign(
                PaymentReversed {
                    settlement_id: SettlementId::new(SETTLEMENT),
                    buyer: peer(&buyer),
                    timestamp: Timestamp::from_millis(1_300),
                },
                &buyer,
            )),
        );
        assert!(
            refused.is_err(),
            "a buyer must not be able to withdraw a declaration the merchant has already \
             answered with {expected:?}"
        );
        assert_eq!(
            state
                .settlements
                .get(&SettlementId::new(SETTLEMENT))
                .expect("the settlement exists")
                .state,
            expected
        );
    }
}

/// The sharp edge of this method, asserted rather than only written down:
/// reversal returns the settlement to `AwaitingPayment`, and
/// `sendSettlementCancelled` is legal from `AwaitingPayment`. So a buyer
/// who withdraws a declaration for fiat that genuinely left their account
/// has handed the merchant a window to cancel the trade out from under the
/// money.
///
/// This is the documented behaviour, not a bug to fix here — the state
/// machine has one `AwaitingPayment` and reversal means exactly "put it
/// back there". It is a test so that anyone who changes the cancellation
/// rules finds out they have changed this too, and so the warning in the
/// method's own docs cannot quietly stop being true.
#[test]
fn withdrawing_a_declaration_re_arms_the_merchants_cancel() {
    let (table, state, buyer, seller) = a_settlement();
    declare_payment(&table, &state, &buyer);

    let cancel = |ts: u64| {
        send_params(&SignedSettlementCancelled::sign(
            SettlementCancelled {
                settlement_id: SettlementId::new(SETTLEMENT),
                canceller: peer(&seller),
                timestamp: Timestamp::from_millis(ts),
            },
            &seller,
        ))
    };

    assert!(
        table
            .dispatch(&state, SETTLEMENT_CANCELLED, cancel(1_150))
            .is_err(),
        "a declared payment must block the merchant's cancel, or the rest of this test is \
         asserting nothing"
    );

    table
        .dispatch(
            &state,
            PAYMENT_REVERSED,
            send_params(&SignedPaymentReversed::sign(
                PaymentReversed {
                    settlement_id: SettlementId::new(SETTLEMENT),
                    buyer: peer(&buyer),
                    timestamp: Timestamp::from_millis(1_200),
                },
                &buyer,
            )),
        )
        .expect("the buyer withdraws");

    table
        .dispatch(&state, SETTLEMENT_CANCELLED, cancel(1_250))
        .expect(
            "and the merchant's cancel is legal again — this is why a client must confirm \
                 before reversing a declaration for money that actually moved",
        );
    assert_eq!(
        state
            .settlements
            .get(&SettlementId::new(SETTLEMENT))
            .expect("the settlement exists")
            .state,
        SettlementState::Cancelled
    );
}
