//! "You have traded 6 times with this wallet" — one wallet's own trading
//! history, folded per counterparty.
//!
//! Like the rest of this crate, and like `openfiat-reputation`, this is a
//! read-time aggregate over state that is already replicated: a
//! `Settlement` names its `buyer`, its `seller`, its state and its
//! timestamps, so counting how many times two wallets have settled with
//! each other needs no new record, no new event and nothing new
//! persisted. Adding any of those would create exactly the harvestable
//! "who trades with whom" trail this feature must not leave behind.
//!
//! # This is not a social graph
//!
//! Every value here is derived only from settlements the *asking* wallet
//! is itself a party to. It is their own trade history re-presented, not
//! a view of anybody else's — nothing about a counterparty's other
//! trades, other counterparties, or overall activity is reachable
//! through it. That is also why nothing here is coarsened: the caller
//! was present for every trade counted, already knows the amounts and
//! the dates, and blurring them would degrade a legitimate view without
//! withholding anything.
//!
//! The confidentiality that matters is therefore entirely in *who is
//! allowed to ask*. This module deliberately has no notion of a
//! requester and no way to authenticate one; the wallet-ownership proof
//! that gates it lives at the RPC boundary
//! (`openfiat_rpc::methods::counterparties`), so a bare
//! [`CounterpartyView`] can never be exposed to the network by accident.
//!
//! # What counts as a trade
//!
//! See [`CounterpartySummary`]: the headline count is settlements that
//! reached `Approved` or `Completed`, and every other outcome is
//! reported separately rather than folded in or silently dropped.

use openfiat_settlement::{Settlement, SettlementRegistry, SettlementState};
use openfiat_storage::KvStore;
use openfiat_types::{PeerId, Timestamp};
use std::collections::HashMap;
use std::rc::Rc;

/// Everything one wallet has done with one counterparty.
///
/// `trades`, `in_progress` and `abandoned` partition every settlement
/// between the pair — [`Self::settlements`] is their sum. Keeping them
/// apart is the point: someone deciding whether to deal with a wallet
/// again needs to see that eleven of their twelve settlements ended in
/// cancellation, and a single "12" would hide that.
///
/// `disputed` is an overlay on those three, not a fourth bucket, for the
/// reason given on the field.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CounterpartySummary {
    pub counterparty: PeerId,
    /// Settlements that reached `Approved` or `Completed` — the number
    /// "you have traded N times with this wallet" refers to.
    ///
    /// `Approved` counts because it is the last *peer-to-peer* act in a
    /// trade: the merchant has released, and the transition to
    /// `Completed` is recorded only by nodes that independently watch
    /// the chain for the release transaction
    /// (`SettlementRegistry::apply_escrow_released`). Counting
    /// `Completed` alone would make the answer depend on which node you
    /// happened to ask rather than on what actually happened.
    pub trades: u32,
    /// Still live: awaiting payment, or awaiting the merchant's decision.
    pub in_progress: u32,
    /// Ended with no transfer — rejected by the merchant, or cancelled
    /// before payment. Not a fault to hold against anyone on its own,
    /// but not a trade either.
    pub abandoned: u32,
    /// How many of the settlements above were escalated to arbitration
    /// (OFS-2400) — whether the case is still running or has long since
    /// resolved.
    ///
    /// Overlapping rather than exclusive, because it answers a question
    /// about a trade's *history* rather than its state. A live case does
    /// have a state of its own now (`SettlementState::Disputed`, counted
    /// in `in_progress`), but arbitration that concluded leaves the
    /// settlement in `Completed` or `Cancelled` like any other, and
    /// "eleven of your twelve trades with this wallet went to
    /// arbitration" is exactly the thing someone deciding whether to deal
    /// with them again needs to see.
    ///
    /// Read from the settlement's own `disputed_at`, which is why this
    /// view no longer holds the dispute registry: the settlement a wallet
    /// is party to is the only record consulted, so a caller physically
    /// cannot learn about a case they were not in.
    pub disputed: u32,
    /// When the most recent counted trade was approved, or `None` if no
    /// settlement between the pair has reached approval.
    ///
    /// Taken from the merchant's own signed approval timestamp rather
    /// than the settlement's `updated_at`. `updated_at` moves to the
    /// local wall clock when a node observes the on-chain release
    /// (`apply_escrow_released`), which is whenever *that node* happened
    /// to catch up — a node syncing from a snapshot would date every
    /// trade it ever imported to the afternoon it started. The approval
    /// timestamp is signed by a participant and identical everywhere.
    pub last_traded_at: Option<Timestamp>,
}

impl CounterpartySummary {
    fn empty(counterparty: PeerId) -> Self {
        Self {
            counterparty,
            trades: 0,
            in_progress: 0,
            abandoned: 0,
            disputed: 0,
            last_traded_at: None,
        }
    }

    /// Every settlement this pair has ever started together.
    pub fn settlements(&self) -> u32 {
        self.trades + self.in_progress + self.abandoned
    }

    fn record(&mut self, settlement: &Settlement) {
        match settlement.state {
            SettlementState::Approved | SettlementState::Completed => {
                self.trades += 1;
                let concluded_at = settlement
                    .merchant_responded_at
                    .unwrap_or(settlement.updated_at);
                self.last_traded_at = Some(match self.last_traded_at {
                    Some(previous) => previous.max(concluded_at),
                    None => concluded_at,
                });
            }
            // `Disputed` belongs with the live states: a frozen escrow in
            // front of arbitrators is unresolved, which is what all three
            // of these mean. It resolves into one of the buckets above
            // once the chain moves the escrow (OFS-2300 §5a).
            SettlementState::AwaitingPayment
            | SettlementState::PaymentSubmitted
            | SettlementState::Disputed => self.in_progress += 1,
            SettlementState::Rejected | SettlementState::Cancelled => self.abandoned += 1,
        }
        if settlement.disputed_at.is_some() {
            self.disputed += 1;
        }
    }

    /// Ranking key for "traders I deal with often": most trades first,
    /// most recent first among equals. A pair with no completed trade
    /// sorts below every pair that has one, whatever else they have
    /// started together.
    fn rank(&self) -> (u32, u64) {
        (
            self.trades,
            self.last_traded_at.map(Timestamp::as_millis).unwrap_or(0),
        )
    }
}

/// Folds a node's replicated settlements into per-counterparty
/// summaries for one wallet at a time.
///
/// There is deliberately no "all counterparties" or "counterparties of
/// X" entry point. Every method takes the wallet whose history is being
/// read as its first argument, so a caller physically cannot ask this
/// type for a relationship that wallet is not part of.
pub struct CounterpartyView<S> {
    settlements: Rc<SettlementRegistry<S>>,
}

impl<S: KvStore> CounterpartyView<S> {
    pub fn new(settlements: Rc<SettlementRegistry<S>>) -> Self {
        Self { settlements }
    }

    /// Everyone `wallet` has ever started a settlement with, most-traded
    /// first.
    ///
    /// Computed on demand — O(settlements) per call, matching
    /// `ReputationView::profile`. Reflects only what this node has
    /// replicated: a node that joined recently will honestly report
    /// fewer trades than happened, so a count is always "as far as this
    /// node has seen", never an authoritative total.
    pub fn for_wallet(&self, wallet: &PeerId) -> Vec<CounterpartySummary> {
        let mut by_counterparty: HashMap<PeerId, CounterpartySummary> = HashMap::new();
        for settlement in self.settlements.all() {
            let Some(counterparty) = counterparty_of(&settlement, wallet) else {
                continue;
            };
            by_counterparty
                .entry(counterparty.clone())
                .or_insert_with(|| CounterpartySummary::empty(counterparty))
                .record(&settlement);
        }

        let mut summaries: Vec<CounterpartySummary> = by_counterparty.into_values().collect();
        // Descending by rank, then by peer id so two counterparties with
        // an identical record don't swap places between calls.
        summaries.sort_unstable_by(|a, b| {
            b.rank()
                .cmp(&a.rank())
                .then_with(|| a.counterparty.as_bytes().cmp(b.counterparty.as_bytes()))
        });
        summaries
    }

    /// This one pair only, for the inline "you have traded N times with
    /// this wallet" badge.
    ///
    /// Returns a zeroed summary rather than `None` when the two have
    /// never traded: "we have no history" is a real, displayable answer,
    /// and a caller forced to distinguish it from an error would end up
    /// inventing the zero itself.
    pub fn pair(&self, wallet: &PeerId, counterparty: &PeerId) -> CounterpartySummary {
        let mut summary = CounterpartySummary::empty(counterparty.clone());
        if wallet == counterparty {
            return summary;
        }
        for settlement in self.settlements.all() {
            if counterparty_of(&settlement, wallet).as_ref() == Some(counterparty) {
                summary.record(&settlement);
            }
        }
        summary
    }
}

/// The other party to `settlement` from `wallet`'s point of view, or
/// `None` if `wallet` was not in it at all.
///
/// A settlement whose buyer and seller are the same peer yields `None`:
/// trading with yourself is not a relationship, and letting it through
/// would put a wallet in its own suggestions.
fn counterparty_of(settlement: &Settlement, wallet: &PeerId) -> Option<PeerId> {
    if settlement.buyer == settlement.seller {
        return None;
    }
    if &settlement.buyer == wallet {
        return Some(settlement.seller.clone());
    }
    if &settlement.seller == wallet {
        return Some(settlement.buyer.clone());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfiat_advertisements::AdvertisementRegistry;
    use openfiat_crypto::Keypair;
    use openfiat_disputes::events::{DisputeOpen, SignedDisputeOpen};
    use openfiat_disputes::{DisputeId, DisputeRegistry, Resolution};
    use openfiat_network::identity::peer_id_from_public_key;
    use openfiat_reservations::{ReservationId, ReservationRegistry};
    use openfiat_settlement::events::{
        PaymentSubmitted, SettlementApproved, SettlementCancelled, SettlementInitiate,
        SettlementRejected, SignedPaymentSubmitted, SignedSettlementApproved,
        SignedSettlementCancelled, SignedSettlementInitiate, SignedSettlementRejected,
    };
    use openfiat_settlement::{PaymentDiscrepancy, SettlementId};
    use openfiat_storage::mem::MemoryStore;
    use openfiat_types::Amount;

    /// Every settlement in these tests is driven through the real signed
    /// events, so a state a test asserts on is a state the protocol can
    /// actually reach. `Disputed` was absent from this list for exactly
    /// that reason until OFS-2300 §5a gave it a writer; it is here now,
    /// and it is reached the only way it can be — by opening a real
    /// dispute against a real settlement.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Outcome {
        AwaitingPayment,
        PaymentSubmitted,
        Approved,
        Completed,
        Rejected,
        Cancelled,
        Disputed,
    }

    struct Fixture {
        settlements: Rc<SettlementRegistry<MemoryStore>>,
        disputes: Rc<DisputeRegistry<MemoryStore>>,
        next_id: usize,
    }

    impl Fixture {
        fn new() -> Self {
            // These tests fold settlements; the reservations behind them
            // are another crate's subject. None is ever created, so §5a's
            // reservation transitions find nothing and no-op — which is
            // itself the behaviour a node needs when a settlement arrives
            // ahead of the reservation it names.
            let reservations = Rc::new(ReservationRegistry::new(
                MemoryStore::new(),
                Rc::new(AdvertisementRegistry::new(MemoryStore::new())),
            ));
            let settlements = Rc::new(SettlementRegistry::new(MemoryStore::new(), reservations));
            let disputes = Rc::new(DisputeRegistry::new(
                MemoryStore::new(),
                Rc::clone(&settlements),
            ));
            Self {
                settlements,
                disputes,
                next_id: 0,
            }
        }

        fn view(&self) -> CounterpartyView<MemoryStore> {
            CounterpartyView::new(Rc::clone(&self.settlements))
        }

        fn settle(
            &mut self,
            buyer: &Keypair,
            seller: &Keypair,
            outcome: Outcome,
            at: Timestamp,
        ) -> SettlementId {
            self.next_id += 1;
            let id = SettlementId::new(format!("settle-{}", self.next_id));
            let buyer_id = peer(buyer);
            let seller_id = peer(seller);
            self.settlements
                .apply_initiate(SignedSettlementInitiate::sign(
                    SettlementInitiate {
                        id: id.clone(),
                        reservation_id: ReservationId::new(format!("res-{}", self.next_id)),
                        buyer: buyer_id.clone(),
                        buyer_public_key: buyer.public_key(),
                        seller: seller_id.clone(),
                        seller_public_key: seller.public_key(),
                        amount: Amount::new(2_000_000, 6),
                        timestamp: at,
                    },
                    buyer,
                ))
                .expect("a fresh settlement is always accepted");

            if outcome == Outcome::AwaitingPayment {
                return id;
            }
            if outcome == Outcome::Cancelled {
                self.settlements
                    .apply_cancelled(SignedSettlementCancelled::sign(
                        SettlementCancelled {
                            settlement_id: id.clone(),
                            canceller: buyer_id,
                            timestamp: at,
                        },
                        buyer,
                    ))
                    .expect("cancelling before payment is legal");
                return id;
            }

            self.settlements
                .apply_payment_submitted(SignedPaymentSubmitted::sign(
                    PaymentSubmitted {
                        settlement_id: id.clone(),
                        buyer: buyer_id,
                        payment_reference: None,
                        timestamp: at,
                    },
                    buyer,
                ))
                .expect("declaring payment is legal from AwaitingPayment");

            match outcome {
                Outcome::PaymentSubmitted => {}
                Outcome::Rejected => {
                    self.settlements
                        .apply_rejected(SignedSettlementRejected::sign(
                            SettlementRejected {
                                settlement_id: id.clone(),
                                seller: seller_id,
                                reason: "no matching deposit".to_string(),
                                discrepancy: PaymentDiscrepancy::IncorrectAmount,
                                timestamp: at,
                            },
                            seller,
                        ))
                        .expect("rejecting a submitted payment is legal");
                }
                Outcome::Approved | Outcome::Completed => {
                    self.settlements
                        .apply_approved(SignedSettlementApproved::sign(
                            SettlementApproved {
                                settlement_id: id.clone(),
                                seller: seller_id,
                                timestamp: at,
                            },
                            seller,
                        ))
                        .expect("approving a submitted payment is legal");
                    if outcome == Outcome::Completed {
                        self.settlements
                            .apply_escrow_released(&id, "onchain-signature")
                            .expect("release is legal from Approved");
                    }
                }
                Outcome::Disputed => {
                    self.dispute(&id, buyer, at);
                }
                Outcome::AwaitingPayment | Outcome::Cancelled => unreachable!("handled above"),
            }
            id
        }

        fn dispute(
            &mut self,
            settlement_id: &SettlementId,
            opener: &Keypair,
            at: Timestamp,
        ) -> DisputeId {
            self.next_id += 1;
            self.disputes
                .apply_open(SignedDisputeOpen::sign(
                    DisputeOpen {
                        id: DisputeId::new(format!("dispute-{}", self.next_id)),
                        settlement_id: settlement_id.clone(),
                        opener: peer(opener),
                        opener_public_key: opener.public_key(),
                        reason: "funds never arrived".to_string(),
                        timestamp: at,
                    },
                    opener,
                ))
                .expect("a party to the settlement may open a dispute")
        }

        /// The chain executed the case and this node observed it — the
        /// only thing that resolves a dispute, and now the only thing
        /// that moves the settlement back out of `Disputed`.
        fn resolve(&self, dispute_id: &DisputeId, outcome: Resolution) {
            self.disputes
                .apply_onchain_execution(dispute_id, "dispute-execution-signature", Some(outcome))
                .expect("an unresolved case records the chain's outcome");
        }

        fn state_of(&self, settlement_id: &SettlementId) -> SettlementState {
            self.settlements
                .get(settlement_id)
                .expect("the settlement exists")
                .state
        }
    }

    fn at(millis: u64) -> Timestamp {
        Timestamp::from_millis(millis)
    }

    fn peer(keypair: &Keypair) -> PeerId {
        peer_id_from_public_key(&keypair.public_key()).unwrap()
    }

    #[test]
    fn counts_only_approved_and_completed_settlements_as_trades() {
        let mut fixture = Fixture::new();
        let me = Keypair::generate();
        let them = Keypair::generate();

        fixture.settle(&me, &them, Outcome::Completed, at(1_000));
        fixture.settle(&me, &them, Outcome::Completed, at(2_000));
        fixture.settle(&me, &them, Outcome::Approved, at(3_000));
        fixture.settle(&me, &them, Outcome::AwaitingPayment, at(4_000));
        fixture.settle(&me, &them, Outcome::PaymentSubmitted, at(5_000));
        fixture.settle(&me, &them, Outcome::Rejected, at(6_000));
        fixture.settle(&me, &them, Outcome::Cancelled, at(7_000));

        let summary = fixture.view().pair(&peer(&me), &peer(&them));
        assert_eq!(summary.trades, 3, "two completed plus one approved");
        assert_eq!(summary.in_progress, 2);
        assert_eq!(summary.abandoned, 2, "rejected and cancelled");
        assert_eq!(
            summary.settlements(),
            7,
            "the three buckets partition every settlement"
        );
        assert_eq!(summary.disputed, 0);
    }

    /// The recency a "you last traded in March" line would show has to
    /// follow the trades, not the most recent thing that happened —
    /// otherwise a cancellation yesterday makes a year-old relationship
    /// look current.
    #[test]
    fn recency_tracks_counted_trades_not_the_newest_settlement() {
        let mut fixture = Fixture::new();
        let me = Keypair::generate();
        let them = Keypair::generate();

        fixture.settle(&me, &them, Outcome::Completed, at(3_000));
        fixture.settle(&me, &them, Outcome::Cancelled, at(9_000));

        assert_eq!(
            fixture.view().pair(&peer(&me), &peer(&them)).last_traded_at,
            Some(at(3_000))
        );
    }

    #[test]
    fn the_same_count_is_reported_to_both_sides_of_the_pair() {
        let mut fixture = Fixture::new();
        let buyer = Keypair::generate();
        let seller = Keypair::generate();
        for i in 1..=6 {
            fixture.settle(&buyer, &seller, Outcome::Completed, at(i * 1_000));
        }

        let view = fixture.view();
        assert_eq!(view.pair(&peer(&buyer), &peer(&seller)).trades, 6);
        assert_eq!(
            view.pair(&peer(&seller), &peer(&buyer)).trades,
            6,
            "the merchant sees the same six trades the buyer does"
        );
    }

    /// Arbitration is an overlay on the three buckets rather than a
    /// fourth: a case that has concluded leaves the settlement in an
    /// ordinary terminal state, so counting it exclusively would take the
    /// trade out of the count it belongs in.
    #[test]
    fn an_arbitrated_trade_is_counted_once_as_a_trade_and_once_as_disputed() {
        let mut fixture = Fixture::new();
        let me = Keypair::generate();
        let them = Keypair::generate();

        fixture.settle(&me, &them, Outcome::Completed, at(1_000));
        let contested = fixture.settle(&me, &them, Outcome::PaymentSubmitted, at(2_000));
        fixture.dispute(&contested, &me, at(2_500));

        let summary = fixture.view().pair(&peer(&me), &peer(&them));
        assert_eq!(summary.trades, 1);
        assert_eq!(summary.in_progress, 1);
        assert_eq!(summary.disputed, 1);
        assert_eq!(
            summary.settlements(),
            2,
            "the dispute overlays the buckets rather than adding to them"
        );
    }

    /// The state a settlement is in while arbitrators are looking at it.
    ///
    /// `SettlementState::Disputed` was declared, documented, and written
    /// by nothing: opening a dispute left the settlement wherever it was,
    /// so a trade in front of arbitrators was indistinguishable from one
    /// waiting on a merchant who had not got round to it. Everything a
    /// user is told about a frozen escrow hangs off this being written.
    #[test]
    fn opening_a_dispute_freezes_the_settlement_it_is_about() {
        let mut fixture = Fixture::new();
        let me = Keypair::generate();
        let them = Keypair::generate();

        let contested = fixture.settle(&me, &them, Outcome::Disputed, at(1_000));

        assert_eq!(
            fixture.state_of(&contested),
            SettlementState::Disputed,
            "a settlement in front of arbitrators must say so"
        );
        let summary = fixture.view().pair(&peer(&me), &peer(&them));
        assert_eq!(summary.in_progress, 1, "a frozen escrow is unresolved");
        assert_eq!(summary.trades, 0);
        assert_eq!(summary.disputed, 1);
    }

    /// The other half, and the half that makes writing `Disputed` safe to
    /// do at all: a settlement has to be able to leave it.
    ///
    /// Without an exit every arbitrated trade would strand — dispute
    /// resolution terminates on the dispute record, and
    /// `apply_escrow_released` only accepts `Approved`, so a buyer who won
    /// their case could never have the release recorded against the
    /// settlement it belongs to.
    #[test]
    fn arbitration_moves_the_settlement_to_the_outcome_the_chain_executed() {
        let mut fixture = Fixture::new();
        let me = Keypair::generate();
        let them = Keypair::generate();

        let won = fixture.settle(&me, &them, Outcome::PaymentSubmitted, at(1_000));
        let case = fixture.dispute(&won, &me, at(1_500));
        fixture.resolve(&case, Resolution::BuyerWins);

        let lost = fixture.settle(&me, &them, Outcome::PaymentSubmitted, at(2_000));
        let case = fixture.dispute(&lost, &me, at(2_500));
        fixture.resolve(&case, Resolution::MerchantWins);

        assert_eq!(
            fixture.state_of(&won),
            SettlementState::Completed,
            "the escrow was released to the buyer, which is a completed trade whatever \
             route it took"
        );
        assert_eq!(
            fixture
                .settlements
                .get(&won)
                .expect("the settlement exists")
                .escrow_release_signature,
            Some("dispute-execution-signature".to_string()),
            "the transaction that released the escrow is the one recorded against it"
        );
        assert_eq!(
            fixture.state_of(&lost),
            SettlementState::Cancelled,
            "the escrow went back to the merchant, so nothing was traded"
        );

        let summary = fixture.view().pair(&peer(&me), &peer(&them));
        assert_eq!(summary.trades, 1);
        assert_eq!(summary.abandoned, 1);
        assert_eq!(
            summary.in_progress, 0,
            "a resolved case must not go on counting as a live trade"
        );
        assert_eq!(
            summary.disputed, 2,
            "both were arbitrated, and that stays true after they conclude"
        );
    }

    #[test]
    fn a_wallet_only_sees_pairs_it_was_part_of() {
        let mut fixture = Fixture::new();
        let me = Keypair::generate();
        let them = Keypair::generate();
        let stranger_a = Keypair::generate();
        let stranger_b = Keypair::generate();

        fixture.settle(&me, &them, Outcome::Completed, at(1_000));
        for i in 0..5 {
            let id = fixture.settle(&stranger_a, &stranger_b, Outcome::Completed, at(2_000 + i));
            fixture.dispute(&id, &stranger_a, at(3_000 + i));
        }

        let summaries = fixture.view().for_wallet(&peer(&me));
        assert_eq!(
            summaries.len(),
            1,
            "five trades between two other wallets must be invisible here"
        );
        assert_eq!(summaries[0].counterparty, peer(&them));
        assert_eq!(
            summaries[0].disputed, 0,
            "somebody else's disputes must not leak in through the overlay"
        );

        assert_eq!(
            fixture
                .view()
                .pair(&peer(&me), &peer(&stranger_a))
                .settlements(),
            0,
            "asking about a stranger's pair must reveal nothing about their trades"
        );
    }

    #[test]
    fn suggestions_rank_by_trade_count_then_recency() {
        let mut fixture = Fixture::new();
        let me = Keypair::generate();
        let frequent = Keypair::generate();
        let recent = Keypair::generate();
        let occasional = Keypair::generate();

        for i in 0..4 {
            fixture.settle(&me, &frequent, Outcome::Completed, at(1_000 + i));
        }
        fixture.settle(&me, &recent, Outcome::Completed, at(9_000));
        fixture.settle(&me, &occasional, Outcome::Completed, at(2_000));

        let ranked = fixture.view().for_wallet(&peer(&me));
        assert_eq!(ranked[0].counterparty, peer(&frequent), "four trades first");
        assert_eq!(
            ranked[1].counterparty,
            peer(&recent),
            "one trade each, the more recent one first"
        );
        assert_eq!(ranked[2].counterparty, peer(&occasional));
    }

    #[test]
    fn a_counterparty_with_no_completed_trade_still_appears_but_ranks_last() {
        let mut fixture = Fixture::new();
        let me = Keypair::generate();
        let traded = Keypair::generate();
        let only_cancelled = Keypair::generate();

        fixture.settle(&me, &traded, Outcome::Completed, at(1_000));
        for i in 0..3 {
            fixture.settle(&me, &only_cancelled, Outcome::Cancelled, at(5_000 + i));
        }

        let ranked = fixture.view().for_wallet(&peer(&me));
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].counterparty, peer(&traded));
        assert_eq!(ranked[1].counterparty, peer(&only_cancelled));
        assert_eq!(ranked[1].trades, 0);
        assert_eq!(ranked[1].abandoned, 3);
    }

    #[test]
    fn a_wallet_that_has_never_traded_gets_an_empty_list_not_an_error() {
        let fixture = Fixture::new();
        let nobody = Keypair::generate();
        assert!(fixture.view().for_wallet(&peer(&nobody)).is_empty());

        let summary = fixture
            .view()
            .pair(&peer(&nobody), &peer(&Keypair::generate()));
        assert_eq!(summary.trades, 0);
        assert_eq!(summary.last_traded_at, None);
    }

    #[test]
    fn a_wallet_is_never_its_own_counterparty() {
        let mut fixture = Fixture::new();
        let me = Keypair::generate();
        fixture.settle(&me, &me, Outcome::Completed, at(1_000));

        assert!(
            fixture.view().for_wallet(&peer(&me)).is_empty(),
            "trading with yourself is not a relationship"
        );
        assert_eq!(fixture.view().pair(&peer(&me), &peer(&me)).trades, 0);
    }
}
