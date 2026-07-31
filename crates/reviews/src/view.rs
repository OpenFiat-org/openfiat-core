//! Reading reviews by wallet, and what a stranger is shown of one.
//!
//! # A review is an edge, and this network hides edges
//!
//! `openfiat_rpc::methods::redaction` removed the parties from every
//! public trade record, on the stated grounds that "who trades with whom"
//! is a physical-safety question in a P2P fiat market: which merchant a
//! wallet always returns to, and who a busy merchant's regulars are.
//! `methods::counterparties` refuses the same aggregate to anyone who
//! cannot prove they are the wallet being asked about.
//!
//! A review names its author and — through its settlement — its subject.
//! Published whole, it hands back exactly the edge those two decisions
//! removed, one review at a time, and it would do so *permanently*,
//! because a review is stored forever whereas a settlement at least
//! stops being interesting. Adding this feature without answering that
//! would have silently undone a fix this project already made.
//!
//! So there are two reads, and the difference between them is the whole
//! privacy answer:
//!
//! - [`ReviewsView::public_about`] — open to anyone, and the shape it
//!   returns is [`PublicReview`]: the subject, the stars, the words, and
//!   the day. Not the author, and not the settlement. A stranger learns
//!   that *somebody who traded with this wallet* said this, which is the
//!   entire useful content of a review, and learns nothing about who that
//!   was.
//! - [`ReviewsView::involving`] — the full records, for a wallet that has
//!   proved it is one of the two people in them. A party already knows
//!   who they traded with; nothing is disclosed to them that they were
//!   not present for.
//!
//! # Why the settlement id goes too
//!
//! Dropping only the author would not work. Both parties may review the
//! same trade, so a public reader who could see the settlement id would
//! find one review of A and one of B carrying the same id, and the edge
//! A-B is back. Once the id is gone, two reviews of the same trade are
//! two unrelated rows about two different wallets.
//!
//! # What remains, honestly
//!
//! The timestamp is truncated to the day, because two reviews published
//! within the same minute are a weak correlation between their subjects.
//! The day is a much weaker one, and it is still all a reader needs to
//! know how recent an opinion is.
//!
//! Beyond that, two things this does not claim. The comment is free text
//! and its author may put a name in it — that is the author's own choice
//! about their own words, unlike a payment reference that was captured
//! for another purpose entirely, and it is why a client must warn before
//! it publishes. And none of this is confidentiality: reviews are gossiped
//! to every node, so anyone running one reads the raw records. What is
//! protected is the ease of the query — the difference between `curl`-ing
//! a stranger's access node and standing up a node to index the network —
//! which is the same thing, and the same amount, that `redaction` and
//! `counterparties` protect.

use crate::record::{PublishedReview, Rating, subject_of};
use crate::store::ReviewRegistry;
use openfiat_settlement::SettlementRegistry;
use openfiat_storage::KvStore;
use openfiat_types::{PeerId, Timestamp};
use std::rc::Rc;

/// What a stranger sees of a review: the opinion, without the edge.
///
/// The rule for adding a field is `redaction`'s, and for the same reason:
/// a field belongs here only if it says something about the *trade* rather
/// than about the *people*. Adding one later is a release note; removing
/// one is a disclosure that already happened.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PublicReview {
    /// Kept, and it is the point: this is a review *of* somebody, and the
    /// caller asked about that wallet by name. One end of an edge is not
    /// an edge — the same line `PublicReservation` draws when it keeps the
    /// advertisement and drops the requester.
    pub about: PeerId,
    pub rating: Rating,
    pub comment: String,
    /// Midnight UTC of the day the review was written — see the module
    /// doc on why the exact moment is not published.
    pub created_on: Timestamp,
}

const MILLIS_PER_DAY: u64 = 24 * 60 * 60 * 1_000;

impl PublicReview {
    fn from(review: PublishedReview) -> Self {
        Self {
            about: review.about,
            rating: review.rating,
            comment: review.comment,
            created_on: Timestamp::from_millis(
                review.created_at.as_millis() - review.created_at.as_millis() % MILLIS_PER_DAY,
            ),
        }
    }
}

/// Reviews joined against the settlements that authorize them.
///
/// Reviews are the one thing in this crate that cannot be read without
/// the settlement registry, which is why the join lives in a view rather
/// than in the store: the store answers "what was published", the view
/// answers "what counts", and only the second is ever shown to anybody.
pub struct ReviewsView<S> {
    reviews: Rc<ReviewRegistry<S>>,
    settlements: Rc<SettlementRegistry<S>>,
}

impl<S: KvStore> ReviewsView<S> {
    pub fn new(reviews: Rc<ReviewRegistry<S>>, settlements: Rc<SettlementRegistry<S>>) -> Self {
        Self {
            reviews,
            settlements,
        }
    }

    /// Every review this node holds that its settlement actually
    /// authorizes, newest first.
    ///
    /// Computed on demand — O(reviews) settlement lookups per call, the
    /// same shape and the same trade-off as `ReputationView::profile`.
    /// Fine at the scale a single node's replica holds; if that stops
    /// being true, maintain a per-wallet index incrementally rather than
    /// rescanning.
    ///
    /// Newest first, unlike `ReviewRegistry::find_for_settlement`'s
    /// oldest-first: one trade's two reviews are a transcript, and a
    /// wallet's reviews are a feed.
    fn authorized(&self) -> Vec<PublishedReview> {
        let mut published: Vec<PublishedReview> = self
            .reviews
            .all()
            .into_iter()
            .filter_map(|review| {
                let settlement = self.settlements.get(&review.settlement)?;
                let about = subject_of(&settlement, &review.author)?;
                Some(PublishedReview::new(review, about))
            })
            .collect();
        published.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        published
    }

    /// Reviews written about `wallet` by the people who traded with it,
    /// in full. Not for a public surface — see [`Self::public_about`].
    pub fn about(&self, wallet: &PeerId) -> Vec<PublishedReview> {
        self.authorized()
            .into_iter()
            .filter(|review| &review.about == wallet)
            .collect()
    }

    /// What anyone may read about `wallet`.
    pub fn public_about(&self, wallet: &PeerId) -> Vec<PublicReview> {
        self.about(wallet)
            .into_iter()
            .map(PublicReview::from)
            .collect()
    }

    /// Every review `wallet` wrote or is the subject of, in full — for a
    /// caller that has proved it holds `wallet`.
    ///
    /// Both directions in one list because both are things this wallet was
    /// present for: it wrote the one, and it was the counterparty in the
    /// trade behind the other. It is also what a client needs to answer
    /// "have I already reviewed this trade?" without asking the network
    /// about anybody else.
    pub fn involving(&self, wallet: &PeerId) -> Vec<PublishedReview> {
        self.authorized()
            .into_iter()
            .filter(|review| &review.author == wallet || &review.about == wallet)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::SignedReviewPublish;
    use crate::fixtures::{keypair, other, peer, review_at};
    use openfiat_settlement::SettlementId;
    use openfiat_settlement::events::{
        PaymentSubmitted, SettlementApproved, SettlementInitiate, SignedPaymentSubmitted,
        SignedSettlementApproved, SignedSettlementInitiate,
    };
    use openfiat_storage::mem::MemoryStore;
    use openfiat_types::Amount;

    type Store = Rc<MemoryStore>;

    fn view() -> (ReviewsView<Store>, Rc<ReviewRegistry<Store>>) {
        let store = Rc::new(MemoryStore::new());
        let reviews = Rc::new(ReviewRegistry::new(Rc::clone(&store)));
        let settlements = Rc::new(SettlementRegistry::new(Rc::clone(&store)));
        (ReviewsView::new(Rc::clone(&reviews), settlements), reviews)
    }

    /// Drives the real settlement events rather than writing a record
    /// directly, so the state the authorization turns on is one the
    /// protocol actually produces.
    fn settle(view: &ReviewsView<Store>, id: &str) {
        let settlement_id = SettlementId::new(id);
        let at = Timestamp::from_millis(100);
        view.settlements
            .apply_initiate(SignedSettlementInitiate::sign(
                SettlementInitiate {
                    id: settlement_id.clone(),
                    reservation_id: openfiat_reservations::ReservationId::new(format!("res-{id}")),
                    buyer: peer(1),
                    buyer_public_key: keypair(1).public_key(),
                    seller: peer(2),
                    seller_public_key: keypair(2).public_key(),
                    amount: Amount::new(1_000_000, 6),
                    timestamp: at,
                },
                &keypair(1),
            ))
            .unwrap();
        view.settlements
            .apply_payment_submitted(SignedPaymentSubmitted::sign(
                PaymentSubmitted {
                    settlement_id: settlement_id.clone(),
                    buyer: peer(1),
                    payment_reference: None,
                    timestamp: at,
                },
                &keypair(1),
            ))
            .unwrap();
        view.settlements
            .apply_approved(SignedSettlementApproved::sign(
                SettlementApproved {
                    settlement_id,
                    seller: peer(2),
                    timestamp: at,
                },
                &keypair(2),
            ))
            .unwrap();
    }

    fn publish(
        reviews: &ReviewRegistry<Store>,
        settlement: &str,
        author_seed: u8,
        rating: Rating,
        comment: &str,
        at_millis: u64,
    ) {
        reviews
            .apply_publish(SignedReviewPublish::sign(
                review_at(settlement, author_seed, rating, comment, at_millis),
                &keypair(author_seed),
            ))
            .unwrap();
    }

    #[test]
    fn a_wallets_reviews_are_the_ones_its_counterparties_wrote_about_it() {
        let (view, reviews) = view();
        settle(&view, "s-1");
        publish(&reviews, "s-1", 1, Rating::Five, "released fast", 1_000);
        publish(&reviews, "s-1", 2, Rating::Two, "paid late", 2_000);

        let about_seller = view.about(&peer(2));
        assert_eq!(about_seller.len(), 1);
        assert_eq!(about_seller[0].comment, "released fast");
        assert_eq!(about_seller[0].author, peer(1));

        let about_buyer = view.about(&peer(1));
        assert_eq!(about_buyer.len(), 1);
        assert_eq!(about_buyer[0].comment, "paid late");
    }

    /// The adversarial case at the surface a client actually reads: a
    /// stranger's review must not appear under the wallet it slanders,
    /// nor anywhere else.
    #[test]
    fn a_review_by_someone_who_never_traded_with_the_wallet_is_shown_to_nobody() {
        let (view, reviews) = view();
        settle(&view, "s-1");
        publish(&reviews, "s-1", 9, Rating::One, "scammer, avoid", 1_000);

        assert!(view.about(&peer(1)).is_empty());
        assert!(view.about(&peer(2)).is_empty());
        assert!(view.public_about(&peer(2)).is_empty());
        assert!(view.involving(&other()).is_empty());
        assert_eq!(
            reviews.all().len(),
            1,
            "it is stored, because gossip may deliver it before the \
             settlement — being stored is not being believed"
        );
    }

    #[test]
    fn a_review_of_a_settlement_this_node_has_never_seen_is_shown_to_nobody() {
        let (view, reviews) = view();
        publish(&reviews, "unknown", 1, Rating::Five, "great", 1_000);
        assert!(view.about(&peer(2)).is_empty());
    }

    /// The privacy answer, asserted on the serialized form rather than
    /// the struct: a field added to `PublicReview` later would compile
    /// fine and quietly widen what a stranger sees.
    #[test]
    fn a_public_review_names_its_subject_and_neither_its_author_nor_its_trade() {
        let (view, reviews) = view();
        settle(&view, "s-1");
        publish(&reviews, "s-1", 1, Rating::Five, "released fast", 1_000);

        let public = view.public_about(&peer(2));
        let json = serde_json::to_string(&public).unwrap();
        assert!(
            json.contains("released fast"),
            "the opinion survives: {json}"
        );
        assert!(
            json.contains(&peer(2).to_string()),
            "the wallet it is about survives: {json}"
        );
        for leaked in ["author", "settlement", "s-1", &peer(1).to_string()] {
            assert!(
                !json.contains(leaked),
                "{leaked:?} rebuilds the trade graph this network hides: {json}"
            );
        }
    }

    /// Both parties reviewing the same trade is the case that would give
    /// the edge away if the settlement id were public.
    #[test]
    fn two_reviews_of_one_trade_are_two_unrelated_public_rows() {
        let (view, reviews) = view();
        settle(&view, "s-1");
        publish(&reviews, "s-1", 1, Rating::Five, "released fast", 1_000);
        publish(&reviews, "s-1", 2, Rating::Two, "paid late", 1_000);

        let both =
            serde_json::to_string(&[view.public_about(&peer(1)), view.public_about(&peer(2))])
                .unwrap();
        assert!(
            !both.contains("s-1"),
            "nothing may join one wallet's public reviews to the other's: {both}"
        );
    }

    #[test]
    fn a_public_review_is_dated_to_the_day_and_not_to_the_moment() {
        let (view, reviews) = view();
        settle(&view, "s-1");
        // 2021-01-01T00:00:00Z plus thirteen and a half hours.
        let precise = 1_609_459_200_000 + 48_600_000;
        publish(&reviews, "s-1", 1, Rating::Five, "good", precise);

        let public = view.public_about(&peer(2));
        assert_eq!(
            public[0].created_on,
            Timestamp::from_millis(1_609_459_200_000)
        );
    }

    #[test]
    fn a_party_reads_both_what_they_wrote_and_what_was_written_about_them() {
        let (view, reviews) = view();
        settle(&view, "s-1");
        publish(&reviews, "s-1", 1, Rating::Five, "released fast", 1_000);
        publish(&reviews, "s-1", 2, Rating::Two, "paid late", 2_000);

        let mine = view.involving(&peer(1));
        assert_eq!(mine.len(), 2);
        assert!(mine.iter().any(|r| r.author == peer(1)), "what I wrote");
        assert!(mine.iter().any(|r| r.about == peer(1)), "and what I got");
        assert!(
            mine.iter().all(|r| r.settlement.as_str() == "s-1"),
            "a party may see which trade, because they were in it"
        );
    }

    #[test]
    fn a_wallet_that_has_never_been_reviewed_gets_an_empty_list_rather_than_an_error() {
        let (view, _reviews) = view();
        assert!(view.public_about(&other()).is_empty());
    }

    #[test]
    fn the_feed_is_newest_first_on_every_node() {
        let (view, reviews) = view();
        settle(&view, "s-1");
        settle(&view, "s-2");
        publish(&reviews, "s-1", 1, Rating::Five, "older", 1_000);
        publish(&reviews, "s-2", 1, Rating::One, "newer", 5_000);

        let about_seller = view.about(&peer(2));
        assert_eq!(about_seller[0].comment, "newer");
        assert_eq!(about_seller[1].comment, "older");
    }
}
