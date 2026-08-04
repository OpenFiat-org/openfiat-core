//! The codes the node actually puts on the wire when it refuses a
//! request.
//!
//! Written after six mappings were found naming an outcome that had not
//! happened — a live dispute reported as `DISPUTE_CLOSED`, a taken
//! proposal id reported as `INVALID_PROPOSAL`, four unrelated deadlines
//! reported as `SESSION_EXPIRED`. Every one of them compiled, and every
//! per-crate unit test passed, because nothing anywhere asserted which
//! `ErrorCode` a condition produces. The mappings are one-line `match`
//! arms; reverting one is a one-line edit that no other test in this
//! workspace notices.
//!
//! Two of these go through the real dispatch table and read the JSON a
//! client would parse, because that is the only place the whole chain is
//! visible: domain error, `RpcError::Application`, `error.data`. The
//! rest assert the mapping directly, which is where the defect lives.

use openfiat_crypto::{Keypair, seal};
use openfiat_disputes::events::{
    ArbitratorJoin, DisputeOpen, SignedArbitratorJoin, SignedDisputeOpen,
};
use openfiat_disputes::{DisputeId, protocol as dispute_protocol};
use openfiat_governance::events::{ProposalCreate, SignedProposalCreate};
use openfiat_governance::{ProposalCategory, ProposalId};
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_reservations::ReservationId;
use openfiat_rpc::dispatch::{MethodTable, encode_bytes};
use openfiat_rpc::error::RpcError;
use openfiat_rpc::methods::build_table;
use openfiat_rpc::state::NodeState;
use openfiat_settlement::SettlementId;
use openfiat_settlement::events::{SettlementInitiate, SignedSettlementInitiate};
use openfiat_storage::mem::MemoryStore;
use openfiat_types::{Amount, ErrorCode, PeerId, Timestamp};
use serde_json::Value;

fn peer(keypair: &Keypair) -> PeerId {
    peer_id_from_public_key(&keypair.public_key()).expect("a keypair derives a peer id")
}

/// The `data` parameter every `sendX` method takes: base64 of the
/// JSON-encoded, already-signed payload.
fn send_params(payload: &impl serde::Serialize) -> Value {
    let bytes = openfiat_serialization::json::to_bytes(payload).expect("serializes");
    serde_json::json!({ "data": encode_bytes(&bytes) })
}

/// The `error.data` object a client receives, taken through the same
/// conversion `jsonrpc.rs` applies at the response boundary.
fn wire_error(error: RpcError) -> Value {
    error
        .into_json_rpc_error()
        .data
        .expect("an application error carries its OFS-8000 identity")
}

/// A node holding one settlement with a dispute open on it, and a panel
/// filled to `REQUIRED_ARBITRATORS`.
fn node_with_a_full_panel() -> (MethodTable<MemoryStore>, NodeState<MemoryStore>, DisputeId) {
    let table = build_table();
    let state = NodeState::new_for_test(MemoryStore::new());
    let buyer = Keypair::from_seed([31u8; 32]);
    let seller = Keypair::from_seed([32u8; 32]);
    let settlement = SettlementId::new("s-panel-1");
    let dispute = DisputeId::new("d-panel-1");

    state
        .settlements
        .apply_initiate(SignedSettlementInitiate::sign(
            SettlementInitiate {
                id: settlement.clone(),
                reservation_id: ReservationId::new("r-panel-1"),
                buyer: peer(&buyer),
                buyer_public_key: buyer.public_key(),
                seller: peer(&seller),
                seller_public_key: seller.public_key(),
                amount: Amount::new(1_000_000, 6),
                timestamp: Timestamp::from_millis(1_000),
            },
            &buyer,
        ))
        .expect("a well-formed settlement applies");

    table
        .dispatch(
            &state,
            "sendDisputeOpen",
            send_params(&SignedDisputeOpen::sign(
                DisputeOpen {
                    id: dispute.clone(),
                    settlement_id: settlement,
                    opener: peer(&buyer),
                    opener_public_key: buyer.public_key(),
                    reason: "payment sent, escrow not released".into(),
                    timestamp: Timestamp::from_millis(1_100),
                },
                &buyer,
            )),
        )
        .expect("a party may open a dispute on their own settlement");

    for seat in 0..dispute_protocol::REQUIRED_ARBITRATORS {
        let arbitrator = Keypair::from_seed([100 + seat; 32]);
        table
            .dispatch(
                &state,
                "sendArbitratorJoin",
                send_params(&SignedArbitratorJoin::sign(
                    ArbitratorJoin {
                        dispute_id: dispute.clone(),
                        arbitrator: peer(&arbitrator),
                        arbitrator_public_key: arbitrator.public_key(),
                        timestamp: Timestamp::from_millis(1_200 + seat as u64),
                    },
                    &arbitrator,
                )),
            )
            .expect("a seat is open until the panel fills");
    }

    (table, state, dispute)
}

/// The panel is full, and the dispute is very much open.
///
/// This arrived as `DISPUTE_CLOSED` (6002) — a code that states the case
/// is over. It is not: it has just finished seating the three
/// arbitrators who will hear it. A late arbitrator told the dispute
/// closed stops watching a case that is about to be argued, and there
/// was no way to tell that answer apart from a genuinely resolved one.
#[test]
fn a_full_panel_is_reported_as_a_dispute_state_not_a_dead_dispute() {
    let (table, state, dispute) = node_with_a_full_panel();
    let latecomer = Keypair::from_seed([200u8; 32]);

    let error = table
        .dispatch(
            &state,
            "sendArbitratorJoin",
            send_params(&SignedArbitratorJoin::sign(
                ArbitratorJoin {
                    dispute_id: dispute,
                    arbitrator: peer(&latecomer),
                    arbitrator_public_key: latecomer.public_key(),
                    timestamp: Timestamp::from_millis(2_000),
                },
                &latecomer,
            )),
        )
        .expect_err("the panel is full");

    let data = wire_error(error);
    assert_eq!(data["ofsErrorCode"], 6005);
    assert_eq!(data["ofsErrorName"], "INVALID_DISPUTE_STATE");
    // Permanent for this request: the panel does not empty, and a
    // client that keeps asking learns nothing.
    assert_eq!(data["ofsRetryable"], false);
}

/// The id is taken. Nothing is wrong with the proposal.
///
/// This arrived as `INVALID_PROPOSAL` (7004), a verdict on content, for
/// a collision that says nothing about content — and the ordinary way
/// to reach it is an author re-sending after a dropped connection, so
/// the proposal 7004 condemned is usually the author's own, already
/// safely stored.
#[test]
fn a_taken_proposal_id_is_reported_as_a_collision_not_a_bad_proposal() {
    let table = build_table();
    let state = NodeState::new_for_test(MemoryStore::new());
    let author = Keypair::from_seed([41u8; 32]);

    let create = |timestamp| {
        SignedProposalCreate::sign(
            ProposalCreate {
                id: ProposalId::new("p-collision-1"),
                title: "Raise the arbitration quorum".into(),
                summary: "Three seats is thin for high-value trades.".into(),
                category: ProposalCategory::Governance,
                author: peer(&author),
                author_public_key: author.public_key(),
                onchain_proposal_id: None,
                timestamp: Timestamp::from_millis(timestamp),
            },
            &author,
        )
    };

    table
        .dispatch(&state, "sendProposalCreate", send_params(&create(1_000)))
        .expect("the first create takes the id");
    let error = table
        .dispatch(&state, "sendProposalCreate", send_params(&create(1_001)))
        .expect_err("the id is taken");

    let data = wire_error(error);
    assert_eq!(data["ofsErrorCode"], 7005);
    assert_eq!(data["ofsErrorName"], "PROPOSAL_ALREADY_EXISTS");
    assert_eq!(data["ofsRetryable"], false);
}

/// Every remaining mapping that used to name an outcome that had not
/// happened, with the code it used to carry beside it.
///
/// A table rather than a test each, because the failure mode is
/// identical in all of them and the interesting content is the pairing.
/// `openfiat-wallet` is absent only because nothing in this crate's
/// dependency graph reaches it; its own module carries the same check.
#[test]
fn no_code_claims_an_outcome_that_has_not_happened() {
    use openfiat_content::ContentError;
    use openfiat_crypto::seal::SealError;
    use openfiat_discovery::DiscoveryError;
    use openfiat_disputes::DisputeError;
    use openfiat_gossip::GossipError;
    use openfiat_governance::GovernanceError;
    use openfiat_oracles::OracleError;
    use openfiat_registry::settlement::FeeSettlementError;
    use openfiat_sessions::SessionError;
    use openfiat_tradechannel::TradeChannelError;

    // (what happened, the code that now says so, the code that used to)
    let mappings: &[(ErrorCode, ErrorCode, &str)] = &[
        (
            DisputeError::InvalidStateTransition.code(),
            ErrorCode::InvalidDisputeState,
            "DISPUTE_CLOSED, for a dispute nobody had closed",
        ),
        (
            DisputeError::ArbitrationFull.code(),
            ErrorCode::InvalidDisputeState,
            "DISPUTE_CLOSED, for a panel that had just filled",
        ),
        (
            GovernanceError::Unauthorized.code(),
            ErrorCode::InvalidIdentityClaim,
            "INVALID_PROPOSAL, for a proposal that was someone else's",
        ),
        (
            GovernanceError::InvalidStateTransition.code(),
            ErrorCode::InvalidProposalState,
            "INVALID_PROPOSAL, for a status that forbade the action",
        ),
        (
            SessionError::AlreadyRevoked.code(),
            ErrorCode::SessionRevoked,
            "SESSION_EXPIRED, for a session revoked on purpose",
        ),
        (
            OracleError::AlreadyExpired.code(),
            ErrorCode::InvalidParameter,
            "SESSION_EXPIRED, in a crate that has no sessions",
        ),
        (
            FeeSettlementError::QuoteExpired.code(),
            ErrorCode::RequestExpired,
            "SESSION_EXPIRED, for a quote that outlived its window",
        ),
        (
            GossipError::IdentityInUseElsewhere.code(),
            ErrorCode::IdentityInUseElsewhere,
            "INVALID_SIGNATURE, for a signature that verified",
        ),
        (
            TradeChannelError::PayloadDidNotOpen.code(),
            ErrorCode::DecryptionFailed,
            "INVALID_SIGNATURE, for an entry whose signature verified",
        ),
        (
            SealError::Failed.code(),
            ErrorCode::DecryptionFailed,
            "INVALID_SIGNATURE, for a box opened with the wrong key",
        ),
        (
            DiscoveryError::InvalidPublicKey.code(),
            ErrorCode::InvalidParameter,
            "INVALID_SIGNATURE, before any signature was looked at",
        ),
        (
            ContentError::NotAParty.code(),
            ErrorCode::InvalidIdentityClaim,
            "INVALID_EVIDENCE, for an author whose peer id did not match",
        ),
    ];

    for (actual, expected, was) in mappings {
        assert_eq!(
            actual,
            expected,
            "expected {} ({}); this condition used to report {was}",
            expected.name(),
            expected.code(),
        );
    }
}

/// The one mapping the panel test above cannot reach: a reader without
/// the channel key.
///
/// `PayloadDidNotOpen` used to be `INVALID_SIGNATURE`, which told a peer
/// missing a key grant to sign the entry again. The entry's signature
/// was never in question — it is verified before decryption is
/// attempted at all.
#[test]
fn a_sealed_payload_that_did_not_open_says_nothing_about_a_signature() {
    let holder = Keypair::from_seed([51u8; 32]);
    let stranger = Keypair::from_seed([52u8; 32]);
    let sealed = seal(&holder.public_key(), b"Equity Bank 0110123456789").expect("seals");

    let error = openfiat_crypto::seal::open(&stranger, &sealed).expect_err("the key does not fit");

    assert_eq!(error.code(), ErrorCode::DecryptionFailed);
    assert_eq!(error.code().code(), 10);
    assert_ne!(error.code(), ErrorCode::InvalidSignature);
}
