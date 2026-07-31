//! The replicated local review index.
//!
//! # Why authorization happens on read, not on write
//!
//! Anyone can sign a record naming any settlement, so a review list that
//! showed every record naming a settlement would let a stranger publish
//! opinions about people they have never traded with — which is precisely
//! the thing that would make this feature worse than not having it. Only
//! the buyer and the seller of a settled trade may review it, and that has
//! to be enforced somewhere.
//!
//! It is enforced in [`ReviewRegistry::find_for_settlement`] rather than
//! in [`ReviewRegistry::apply_publish`], for the reason
//! `openfiat_content::store` already sets out: gossip has no ordering
//! guarantee. A node can receive a review before it has received the
//! settlement it refers to — or before the `SettlementApproved` that made
//! the trade reviewable — and a write-time party check would see an
//! unknown or unsettled trade, conclude the author is not a party, and
//! discard a genuine review permanently. A discarded event is never
//! retried; an unauthorized record that is simply never returned to anyone
//! is inert.
//!
//! So [`apply_publish`] enforces everything checkable from the record
//! alone — signature, authorship, shape — and [`find_for_settlement`]
//! enforces the one fact that needs state. A caller cannot accidentally
//! skip the second check: this registry has no method that returns a
//! review without being handed the settlement it belongs to, and
//! [`crate::record::subject_of`] is the only thing that decides.
//!
//! [`apply_publish`]: ReviewRegistry::apply_publish
//! [`find_for_settlement`]: ReviewRegistry::find_for_settlement

use crate::error::ReviewError;
use crate::events::SignedReviewPublish;
use crate::protocol;
use crate::record::{PublishedReview, Review, ReviewId, subject_of};
use openfiat_serialization::wire;
use openfiat_settlement::Settlement;
use openfiat_storage::KvStore;
use openfiat_types::EventEnvelope;

/// This crate's column family.
///
/// Public because a node's composition root has to list every column
/// family it opens *before* any of them is written to — see
/// `openfiat_rpc::state::SNAPSHOT_COLUMN_FAMILIES`. A registry whose
/// family is missing from that list writes into nothing on a real RocksDB
/// node while passing every in-memory test, so the name is exported
/// rather than spelled twice.
pub const COLUMN_FAMILY: &str = "reviews";

pub struct ReviewRegistry<S> {
    store: S,
}

impl<S: KvStore> ReviewRegistry<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }

    /// The raw record, with no authorization applied — it says who wrote
    /// it and about which trade, but not who it is about, because that is
    /// not knowable without the settlement.
    pub fn get(&self, id: &ReviewId) -> Option<Review> {
        let bytes = self
            .store
            .get(COLUMN_FAMILY, id.as_str().as_bytes())
            .ok()
            .flatten()?;
        wire::from_bytes(&bytes).ok()
    }

    fn put(&self, review: &Review) {
        if let Ok(bytes) = wire::to_bytes(review) {
            let _ = self
                .store
                .put(COLUMN_FAMILY, review.id().as_str().as_bytes(), &bytes);
        }
    }

    pub fn all(&self) -> Vec<Review> {
        self.store
            .iter_prefix(COLUMN_FAMILY, &[])
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(_, value)| wire::from_bytes(&value).ok())
            .collect()
    }

    /// Every review of `settlement` that one of its parties is entitled to
    /// have written, each carrying the wallet it is about.
    ///
    /// The settlement record is a required argument rather than something
    /// this registry looks up, so that the authorization check cannot be
    /// forgotten — see the module documentation. Everything else stored
    /// against this settlement (a stranger's record, or a party's record
    /// on a trade that never settled) is silently absent, which is the
    /// truthful answer: there is no review of that trade by that wallet.
    ///
    /// Ordered oldest first, then by id, so two nodes holding the same
    /// records display them in the same order.
    pub fn find_for_settlement(&self, settlement: &Settlement) -> Vec<PublishedReview> {
        let mut found: Vec<PublishedReview> = [&settlement.buyer, &settlement.seller]
            .into_iter()
            .filter_map(|party| {
                let review = self.get(&ReviewId::of(&settlement.id, party))?;
                let about = subject_of(settlement, &review.author)?;
                Some(PublishedReview::new(review, about))
            })
            .collect();
        found.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        found
    }

    /// Stores a signed review after every check that needs no state.
    ///
    /// # Which of two reviews of the same trade by the same wallet wins
    ///
    /// The later-dated one, ties broken by the record's own canonical
    /// bytes. Not "whichever arrived first", which is what every other
    /// store in this workspace does for author-chosen ids — because here
    /// the id is *derived*, so a collision is not an author squatting an
    /// identifier but an author amending their own words, and gossip
    /// delivers the two in whatever order each node happens to hear them.
    /// First-writer-wins would leave different nodes displaying different
    /// reviews of the same trade forever. A deterministic rule over the
    /// records themselves converges everywhere regardless of arrival
    /// order.
    ///
    /// What that permits is bounded: an author may replace their own
    /// review of their own trade, and nothing else. `id()` is derived from
    /// the settlement and the author, both signature-covered, so no record
    /// can ever land on a key belonging to a different wallet.
    pub fn apply_publish(&self, signed: SignedReviewPublish) -> Result<ReviewId, ReviewError> {
        signed.verify()?;
        signed.review.validate()?;
        let id = signed.review.id();
        if let Some(existing) = self.get(&id)
            && !supersedes(&signed.review, &existing)
        {
            return Err(ReviewError::AlreadyReviewed);
        }
        self.put(&signed.review);
        Ok(id)
    }

    pub fn apply_event(&self, event: &EventEnvelope) {
        if event.ofs_spec != protocol::OFS_SPEC
            || event.event_type.as_str() != protocol::EVENT_PUBLISHED
        {
            return;
        }
        if let Ok(signed) = wire::from_bytes(&event.payload) {
            let _ = self.apply_publish(signed);
        }
    }
}

/// A total order over two reviews of the same trade by the same author.
///
/// `created_at` is self-asserted, and that is acceptable here precisely
/// because the only thing an author can reach with it is their own record.
/// The wire-bytes tie-break exists so that two records sharing a
/// timestamp still resolve identically on every node rather than by
/// arrival order.
fn supersedes(incoming: &Review, existing: &Review) -> bool {
    let key = |review: &Review| {
        (
            review.created_at,
            wire::to_bytes(review).unwrap_or_default(),
        )
    };
    key(incoming) > key(existing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{keypair, other, peer, review, review_at, settled_settlement};
    use crate::record::Rating;
    use openfiat_settlement::SettlementState;
    use openfiat_storage::mem::MemoryStore;

    fn publish(
        registry: &ReviewRegistry<MemoryStore>,
        settlement: &str,
        author_seed: u8,
        rating: Rating,
        comment: &str,
    ) -> Result<ReviewId, ReviewError> {
        registry.apply_publish(SignedReviewPublish::sign(
            review(settlement, author_seed, rating, comment),
            &keypair(author_seed),
        ))
    }

    #[test]
    fn both_parties_can_review_each_other_and_each_review_names_the_other() {
        let registry = ReviewRegistry::new(MemoryStore::new());
        publish(&registry, "s-1", 1, Rating::Five, "paid on time").unwrap();
        publish(&registry, "s-1", 2, Rating::Four, "released quickly").unwrap();

        let found = registry.find_for_settlement(&settled_settlement("s-1"));
        assert_eq!(found.len(), 2);
        let buyers = found.iter().find(|r| r.author == peer(1)).unwrap();
        assert_eq!(buyers.about, peer(2), "the buyer's review is of the seller");
        let sellers = found.iter().find(|r| r.author == peer(2)).unwrap();
        assert_eq!(sellers.about, peer(1), "and the seller's is of the buyer");
    }

    /// The load-bearing test for this whole feature. The stranger's
    /// record is perfectly well-formed and genuinely their own — the
    /// signature was never the question. What they are not is a party to
    /// the trade.
    #[test]
    fn a_stranger_cannot_publish_a_review_of_someone_elses_trade() {
        let registry = ReviewRegistry::new(MemoryStore::new());
        publish(&registry, "s-1", 1, Rating::Five, "smooth").unwrap();
        publish(&registry, "s-1", 9, Rating::One, "scammer, avoid").unwrap();

        let found = registry.find_for_settlement(&settled_settlement("s-1"));
        assert_eq!(found.len(), 1, "only the parties' reviews are returned");
        assert_eq!(found[0].author, peer(1));
        assert!(
            found
                .iter()
                .all(|r| r.author != other() && r.about != other()),
            "a wallet outside the trade appears nowhere, as author or subject"
        );
    }

    /// Stated from the other direction, because the previous test would
    /// still pass if the stranger's review were merely mis-attributed
    /// rather than dropped.
    #[test]
    fn a_strangers_review_is_never_attributed_to_either_party() {
        let registry = ReviewRegistry::new(MemoryStore::new());
        publish(&registry, "s-1", 9, Rating::One, "scammer, avoid").unwrap();

        let found = registry.find_for_settlement(&settled_settlement("s-1"));
        assert!(
            found.is_empty(),
            "there is nobody a non-party's opinion could honestly be about"
        );
    }

    #[test]
    fn a_trade_that_never_settled_has_no_reviews_however_they_were_signed() {
        let registry = ReviewRegistry::new(MemoryStore::new());
        publish(&registry, "s-1", 1, Rating::One, "never released").unwrap();

        for state in [
            SettlementState::AwaitingPayment,
            SettlementState::PaymentSubmitted,
            SettlementState::Cancelled,
            SettlementState::Rejected,
            SettlementState::Disputed,
        ] {
            let mut settlement = settled_settlement("s-1");
            settlement.state = state;
            assert!(
                registry.find_for_settlement(&settlement).is_empty(),
                "{state:?} is not a trade anyone completed"
            );
        }
    }

    /// The ordering hazard the module documentation describes: this node
    /// stores the record while knowing nothing about `s-1`, and only
    /// later learns who its parties were and that it settled.
    #[test]
    fn a_review_that_arrived_before_its_settlement_is_not_lost() {
        let registry = ReviewRegistry::new(MemoryStore::new());
        publish(&registry, "s-1", 1, Rating::Five, "good trade").unwrap();

        let mut in_flight = settled_settlement("s-1");
        in_flight.state = SettlementState::PaymentSubmitted;
        assert!(registry.find_for_settlement(&in_flight).is_empty());

        assert_eq!(
            registry
                .find_for_settlement(&settled_settlement("s-1"))
                .len(),
            1,
            "a genuine review must survive arriving out of order"
        );
    }

    #[test]
    fn reviews_of_another_trade_are_not_mixed_in() {
        let registry = ReviewRegistry::new(MemoryStore::new());
        publish(&registry, "s-1", 1, Rating::Five, "first").unwrap();
        publish(&registry, "s-2", 1, Rating::One, "second").unwrap();

        let found = registry.find_for_settlement(&settled_settlement("s-1"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].comment, "first");
    }

    #[test]
    fn a_party_gets_one_review_per_trade_rather_than_a_pile() {
        let registry = ReviewRegistry::new(MemoryStore::new());
        publish(&registry, "s-1", 1, Rating::Five, "great").unwrap();
        assert_eq!(
            registry.apply_publish(SignedReviewPublish::sign(
                review_at("s-1", 1, Rating::One, "actually terrible", 500),
                &keypair(1),
            )),
            Err(ReviewError::AlreadyReviewed),
            "an older record cannot displace the one on file"
        );
        let found = registry.find_for_settlement(&settled_settlement("s-1"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].comment, "great");
    }

    /// Two nodes hear an author's original and their amendment in
    /// opposite orders. They must end up displaying the same review, or
    /// the network disagrees about what somebody said.
    #[test]
    fn two_nodes_converge_on_the_same_review_whatever_order_they_hear_it_in() {
        let first = review_at("s-1", 1, Rating::Five, "great", 1_000);
        let amended = review_at("s-1", 1, Rating::Three, "on reflection, average", 2_000);

        let mut settled = Vec::new();
        for order in [[&first, &amended], [&amended, &first]] {
            let registry = ReviewRegistry::new(MemoryStore::new());
            for review in order {
                let _ =
                    registry.apply_publish(SignedReviewPublish::sign(review.clone(), &keypair(1)));
            }
            settled.push(registry.find_for_settlement(&settled_settlement("s-1")));
        }
        assert_eq!(settled[0], settled[1]);
        assert_eq!(settled[0][0].comment, "on reflection, average");
    }

    #[test]
    fn a_forged_signature_never_reaches_the_store() {
        let registry = ReviewRegistry::new(MemoryStore::new());
        let forged =
            SignedReviewPublish::sign(review("s-1", 1, Rating::One, "not mine"), &keypair(2));
        assert_eq!(
            registry.apply_publish(forged),
            Err(ReviewError::InvalidSignature)
        );
        assert!(registry.all().is_empty());
    }

    #[test]
    fn an_oversized_comment_never_reaches_the_store() {
        let registry = ReviewRegistry::new(MemoryStore::new());
        let huge = "a".repeat(crate::record::MAX_COMMENT_CHARS + 1);
        assert_eq!(
            publish(&registry, "s-1", 1, Rating::Five, &huge),
            Err(ReviewError::MalformedReview),
            "the bound is only worth having if it is applied before storage"
        );
        assert!(registry.all().is_empty());
    }

    #[test]
    fn a_gossiped_event_from_another_spec_is_ignored() {
        let registry = ReviewRegistry::new(MemoryStore::new());
        let payload = wire::to_bytes(&SignedReviewPublish::sign(
            review("s-1", 1, Rating::Five, "good"),
            &keypair(1),
        ))
        .unwrap();
        let mut envelope = EventEnvelope {
            id: openfiat_types::EventId::from_bytes([7; 32]),
            event_type: openfiat_types::EventType::new(protocol::EVENT_PUBLISHED).unwrap(),
            ofs_spec: 9999,
            version: 1,
            origin: peer(1),
            timestamp: openfiat_types::Timestamp::from_millis(1),
            ttl: 8,
            priority: openfiat_types::Priority::Reputation,
            signature: openfiat_types::Signature::from_bytes([0u8; 64]),
            payload,
        };
        registry.apply_event(&envelope);
        assert!(registry.all().is_empty());

        envelope.ofs_spec = protocol::OFS_SPEC;
        registry.apply_event(&envelope);
        assert_eq!(registry.all().len(), 1);
    }
}
