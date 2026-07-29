//! "You have traded 6 times with this wallet" — and the ownership proof
//! that keeps it from being a public social graph.
//!
//! # Why this surface is authenticated at all
//!
//! Every other read method here is open, because everything it returns
//! is already replicated to every node. The aggregate this one returns
//! is different in kind rather than in content: an open endpoint that
//! answers "who does this wallet trade with, and how often" hands anyone
//! a map of real trading relationships — which merchant a wallet always
//! goes back to, who a high-volume merchant's regulars are, and
//! therefore who is worth following home. In a P2P fiat market that is a
//! physical-safety question, not a preference.
//!
//! So there is no unauthenticated counterparty method, and no parameter
//! that widens one wallet's request into anyone else's history. A caller
//! reads their own relationships or none.
//!
//! # How it authenticates
//!
//! The same sign-this-nonce handshake providers use to read their own
//! earnings statement (`methods::providers`), lifted into the reusable
//! [`openfiat_crypto::challenge`] primitive:
//!
//! 1. `getCounterpartiesChallenge` hands out a random, single-use,
//!    expiring nonce bound to one wallet. Deliberately open — a nonce is
//!    worthless without the private key that signs it, and demanding a
//!    signature to obtain the thing you sign would be circular. It
//!    confirms nothing about the wallet, not even that it exists.
//! 2. `getCounterparties` takes that nonce, the caller's public key and
//!    their signature over the challenge's own bytes. The public key
//!    must derive to exactly the wallet being asked about (OFNP §6's
//!    derivation, the same check advertisements use against peer
//!    poisoning), and the signature must verify against it.
//!
//! A request for a wallet the caller cannot prove they control is
//! **refused**, not quietly narrowed to what they are entitled to. A
//! filtering implementation would look identical in every passing test
//! right up until a refactor forgot the filter; a refusal fails loudly.
//!
//! # Nothing new is stored
//!
//! The answer is folded on demand from settlements the node already
//! replicates (`openfiat_trade::counterparties`), and the outstanding
//! challenges live in memory only. A node operator gains no new record
//! of who asked what — which matters, because the operator is exactly
//! the party this feature must not quietly create a dossier for.

use crate::dispatch::{
    MethodTable, WalletParams, decode_bytes, decode_peer_id, encode_peer_id, method_fn,
};
use crate::error::RpcError;
use crate::state::NodeState;
use openfiat_crypto::challenge::{CHALLENGE_TTL_SECS, Challenge};
use openfiat_crypto::verify;
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_storage::KvStore;
use openfiat_trade::CounterpartySummary;
use openfiat_types::{ErrorCode, PublicKey, Signature, Timestamp};

/// Domain separator for these challenges. A signature collected here can
/// never be presented on another challenge-gated surface, even one that
/// happens to identify its subjects by the same base64 peer id.
pub const CHALLENGE_DOMAIN: &str = "openfiat-counterparties";

/// A wallet answering its challenge: whose history, which nonce, the key
/// claiming to be that wallet, and its signature over the challenge's
/// own bytes.
#[derive(serde::Deserialize)]
pub struct CounterpartiesParams {
    /// Base64 `PeerId`, matching every other wallet-scoped method here.
    pub wallet: String,
    /// Base64 raw 32-byte Ed25519 public key. Sent explicitly rather
    /// than recovered from `wallet` so the identity claim is something
    /// the caller states and this method checks, not something the
    /// server infers on their behalf.
    pub public_key: String,
    pub nonce: String,
    /// Base64, matching every other signed payload on this surface.
    pub signature: String,
}

pub fn register<S: KvStore + 'static>(table: &mut MethodTable<S>) {
    table.register(
        "getCounterpartiesChallenge",
        method_fn(
            |state: &NodeState<S>, params: WalletParams| -> Result<Challenge, RpcError> {
                // Normalized through `PeerId` so the subject the caller
                // signs is the canonical encoding of their wallet, not
                // whichever base64 spelling they happened to send.
                let wallet = encode_peer_id(&decode_peer_id(&params.wallet)?);
                Ok(state.counterparty_challenges.borrow_mut().issue(
                    wallet,
                    Timestamp::now(),
                    CHALLENGE_TTL_SECS,
                ))
            },
        ),
    );
    table.register(
        "getCounterparties",
        method_fn(
            |state: &NodeState<S>,
             params: CounterpartiesParams|
             -> Result<Vec<CounterpartySummary>, RpcError> {
                let wallet = decode_peer_id(&params.wallet)?;
                let public_key = decode_public_key(&params.public_key)?;

                // Checked before anything else, and before the nonce is
                // touched: asking for a wallet you do not hold the key
                // for is refused outright, and refusing early also means
                // a stranger's failed attempt cannot spend the nonce its
                // real owner is part-way through signing.
                let claimed = peer_id_from_public_key(&public_key)
                    .map_err(|_| RpcError::Application(ErrorCode::InvalidIdentityClaim))?;
                if claimed != wallet {
                    return Err(RpcError::Application(ErrorCode::InvalidIdentityClaim));
                }

                // Consumed before the signature is checked, so presenting
                // a captured signature burns the nonce rather than
                // replaying it.
                let subject = encode_peer_id(&wallet);
                let challenge = state
                    .counterparty_challenges
                    .borrow_mut()
                    .consume(&subject, &params.nonce, Timestamp::now())
                    .map_err(|e| RpcError::Application(e.code()))?;

                let raw: [u8; 64] = decode_bytes(&params.signature)?
                    .try_into()
                    .map_err(|_| RpcError::InvalidParams("signature must be 64 bytes".into()))?;
                verify(
                    &public_key,
                    &challenge.signing_bytes(CHALLENGE_DOMAIN),
                    &Signature::from_bytes(raw),
                )
                .map_err(|e| RpcError::Application(e.code()))?;

                Ok(state.counterparties.for_wallet(&wallet))
            },
        ),
    );
}

fn decode_public_key(encoded: &str) -> Result<PublicKey, RpcError> {
    let raw: [u8; 32] = decode_bytes(encoded)?
        .try_into()
        .map_err(|_| RpcError::InvalidParams("public key must be 32 bytes".into()))?;
    Ok(PublicKey::from_bytes(raw))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::encode_bytes;
    use openfiat_crypto::Keypair;
    use openfiat_reservations::ReservationId;
    use openfiat_settlement::SettlementId;
    use openfiat_settlement::events::{
        PaymentSubmitted, SettlementApproved, SettlementInitiate, SignedPaymentSubmitted,
        SignedSettlementApproved, SignedSettlementInitiate,
    };
    use openfiat_storage::mem::MemoryStore;
    use openfiat_types::{Amount, PeerId};

    fn table_and_state() -> (MethodTable<MemoryStore>, NodeState<MemoryStore>) {
        let mut table = MethodTable::new();
        register(&mut table);
        (table, NodeState::new_for_test(MemoryStore::new()))
    }

    fn peer(keypair: &Keypair) -> PeerId {
        peer_id_from_public_key(&keypair.public_key()).unwrap()
    }

    /// One approved settlement between the two, applied straight to the
    /// registry the way a gossiped event would.
    fn trade(state: &NodeState<MemoryStore>, buyer: &Keypair, seller: &Keypair, nth: u32) {
        let id = SettlementId::new(format!("settle-{nth}-{}", encode_peer_id(&peer(buyer))));
        let at = Timestamp::from_millis(1_000 + u64::from(nth));
        state
            .settlements
            .apply_initiate(SignedSettlementInitiate::sign(
                SettlementInitiate {
                    id: id.clone(),
                    reservation_id: ReservationId::new(format!("res-{}", id.as_str())),
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
                    settlement_id: id.clone(),
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
                    settlement_id: id,
                    seller: peer(seller),
                    timestamp: at,
                },
                seller,
            ))
            .unwrap();
    }

    fn challenge_for(
        table: &MethodTable<MemoryStore>,
        state: &NodeState<MemoryStore>,
        wallet: &PeerId,
    ) -> Challenge {
        serde_json::from_value(
            table
                .dispatch(
                    state,
                    "getCounterpartiesChallenge",
                    serde_json::json!({ "wallet": encode_peer_id(wallet) }),
                )
                .expect("a challenge is issued for any wallet"),
        )
        .unwrap()
    }

    /// Drives the whole real flow: ask for a challenge on `wallet`, sign
    /// it with `signer`, present the signature.
    fn read_counterparties(
        table: &MethodTable<MemoryStore>,
        state: &NodeState<MemoryStore>,
        wallet: &PeerId,
        signer: &Keypair,
    ) -> Result<Vec<CounterpartySummary>, RpcError> {
        let challenge = challenge_for(table, state, wallet);
        let signature = signer.sign(&challenge.signing_bytes(CHALLENGE_DOMAIN));
        let value = table.dispatch(
            state,
            "getCounterparties",
            serde_json::json!({
                "wallet": encode_peer_id(wallet),
                "public_key": encode_bytes(signer.public_key().as_bytes()),
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
    fn a_wallet_reads_its_own_counterparties_by_signing_the_challenge() {
        let (table, state) = table_and_state();
        let me = Keypair::generate();
        let them = Keypair::generate();
        for nth in 0..6 {
            trade(&state, &me, &them, nth);
        }

        let summaries = read_counterparties(&table, &state, &peer(&me), &me)
            .expect("my own key must be able to read my own counterparties");
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].counterparty, peer(&them));
        assert_eq!(
            summaries[0].trades, 6,
            "you have traded 6 times with this wallet"
        );
    }

    /// The load-bearing test for this whole surface. An authenticated
    /// caller asking about somebody else's wallet must get an **error**.
    /// Asserting on a filtered-but-successful response would keep
    /// passing if a future refactor dropped the scoping and started
    /// returning the victim's real rows.
    #[test]
    fn asking_for_another_wallets_counterparties_is_refused_not_filtered() {
        let (table, state) = table_and_state();
        let victim = Keypair::generate();
        let their_merchant = Keypair::generate();
        for nth in 0..3 {
            trade(&state, &victim, &their_merchant, nth);
        }

        // The attacker holds a perfectly good key and signs correctly —
        // they simply name a wallet that is not theirs.
        let attacker = Keypair::generate();
        let error = read_counterparties(&table, &state, &peer(&victim), &attacker)
            .expect_err("a wallet you cannot prove you control must be refused");
        assert!(
            is(&error, ErrorCode::InvalidIdentityClaim),
            "the refusal must name the identity claim, not look like an empty result: {error:?}"
        );

        // And the victim's history is still intact and still theirs.
        let theirs = read_counterparties(&table, &state, &peer(&victim), &victim).unwrap();
        assert_eq!(theirs[0].trades, 3);
    }

    /// The same refusal, stated from the other direction: whatever the
    /// attacker gets back, it is never a successful response.
    #[test]
    fn no_signature_makes_someone_elses_history_readable() {
        let (table, state) = table_and_state();
        let victim = Keypair::generate();
        let merchant = Keypair::generate();
        trade(&state, &victim, &merchant, 0);

        let attacker = Keypair::generate();
        let victim_wallet = encode_peer_id(&peer(&victim));
        let challenge = challenge_for(&table, &state, &peer(&victim));

        // Every combination an attacker controls: their key with the
        // victim's wallet, the victim's wallet with the attacker's own
        // valid signature over the same challenge, and a claim of the
        // victim's public key they cannot sign for.
        let attempts = [
            serde_json::json!({
                "wallet": victim_wallet,
                "public_key": encode_bytes(attacker.public_key().as_bytes()),
                "nonce": challenge.nonce,
                "signature": encode_bytes(
                    &attacker
                        .sign(&challenge.signing_bytes(CHALLENGE_DOMAIN))
                        .as_bytes()
                        .unwrap(),
                ),
            }),
            serde_json::json!({
                "wallet": victim_wallet,
                "public_key": encode_bytes(victim.public_key().as_bytes()),
                "nonce": challenge.nonce,
                "signature": encode_bytes(
                    &attacker
                        .sign(&challenge.signing_bytes(CHALLENGE_DOMAIN))
                        .as_bytes()
                        .unwrap(),
                ),
            }),
        ];
        for attempt in attempts {
            assert!(
                table
                    .dispatch(&state, "getCounterparties", attempt)
                    .is_err(),
                "no combination an attacker controls may return a result"
            );
        }
    }

    #[test]
    fn a_captured_signature_cannot_be_replayed() {
        let (table, state) = table_and_state();
        let me = Keypair::generate();
        trade(&state, &me, &Keypair::generate(), 0);

        let challenge = challenge_for(&table, &state, &peer(&me));
        let replayed = serde_json::json!({
            "wallet": encode_peer_id(&peer(&me)),
            "public_key": encode_bytes(me.public_key().as_bytes()),
            "nonce": challenge.nonce,
            "signature": encode_bytes(
                &me.sign(&challenge.signing_bytes(CHALLENGE_DOMAIN)).as_bytes().unwrap(),
            ),
        });

        assert!(
            table
                .dispatch(&state, "getCounterparties", replayed.clone())
                .is_ok(),
            "the first presentation is legitimate"
        );
        assert!(
            table
                .dispatch(&state, "getCounterparties", replayed)
                .is_err(),
            "the identical request must fail once the nonce is spent"
        );
    }

    /// Domain separation is only worth having if it is actually enforced,
    /// so this signs the bytes another challenge-gated surface would ask
    /// for and presents them here.
    #[test]
    fn a_signature_from_another_challenge_domain_is_rejected() {
        let (table, state) = table_and_state();
        let me = Keypair::generate();
        let challenge = challenge_for(&table, &state, &peer(&me));

        let wrong_domain = me.sign(&challenge.signing_bytes("openfiat-earnings"));
        let error = table
            .dispatch(
                &state,
                "getCounterparties",
                serde_json::json!({
                    "wallet": encode_peer_id(&peer(&me)),
                    "public_key": encode_bytes(me.public_key().as_bytes()),
                    "nonce": challenge.nonce,
                    "signature": encode_bytes(&wrong_domain.as_bytes().unwrap()),
                }),
            )
            .expect_err("bytes signed for another domain must not authenticate here");
        assert!(is(&error, ErrorCode::InvalidSignature), "{error:?}");
    }

    /// Challenge issuance is open, so it must not be usable to disrupt
    /// somebody mid-handshake.
    #[test]
    fn a_stranger_requesting_challenges_cannot_lock_a_wallet_out() {
        let (table, state) = table_and_state();
        let me = Keypair::generate();
        trade(&state, &me, &Keypair::generate(), 0);

        let mine = challenge_for(&table, &state, &peer(&me));
        for _ in 0..32 {
            challenge_for(&table, &state, &peer(&me));
        }

        let signature = me.sign(&mine.signing_bytes(CHALLENGE_DOMAIN));
        assert!(
            table
                .dispatch(
                    &state,
                    "getCounterparties",
                    serde_json::json!({
                        "wallet": encode_peer_id(&peer(&me)),
                        "public_key": encode_bytes(me.public_key().as_bytes()),
                        "nonce": mine.nonce,
                        "signature": encode_bytes(&signature.as_bytes().unwrap()),
                    }),
                )
                .is_ok(),
            "a flood of anonymous challenge requests must not invalidate mine"
        );
    }

    #[test]
    fn a_wallet_with_no_history_gets_an_empty_list_rather_than_an_error() {
        let (table, state) = table_and_state();
        let nobody = Keypair::generate();
        let summaries = read_counterparties(&table, &state, &peer(&nobody), &nobody)
            .expect("having never traded is not a failure");
        assert!(summaries.is_empty());
    }

    /// Issuing must not become an oracle for "has this wallet ever
    /// traded" — that is exactly the enumeration this surface exists to
    /// prevent, and it would be reintroduced by an existence check.
    #[test]
    fn a_challenge_is_issued_for_a_wallet_that_has_never_been_seen() {
        let (table, state) = table_and_state();
        let stranger = PeerId::from_bytes(vec![9, 9, 9, 9]);
        let challenge = challenge_for(&table, &state, &stranger);
        assert_eq!(challenge.subject, encode_peer_id(&stranger));
    }
}
