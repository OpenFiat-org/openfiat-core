//! The review shape, its bounds, and the one function that decides who
//! may review whom.

use crate::error::ReviewError;
use openfiat_settlement::{Settlement, SettlementId, SettlementState};
use openfiat_types::{PeerId, PublicKey, Timestamp};

/// A review's identifier, derived rather than chosen.
///
/// Every other record in this workspace lets its author pick an id. A
/// review must not, because the id *is* the rule "one review per party
/// per trade": it is exactly `settlement:author`, so a second review of
/// the same trade by the same wallet lands on the same key on every node
/// rather than becoming a second row somebody has to remember to
/// de-duplicate. It also means an author cannot squat an id, and cannot
/// publish under an id that looks like somebody else's.
///
/// The author segment is base58 (`PeerId`'s `Display`), an alphabet with
/// no `:` in it, and it is the final segment — so two different
/// `(settlement, author)` pairs cannot spell the same id however
/// creatively a settlement id is chosen.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct ReviewId(String);

impl ReviewId {
    pub fn of(settlement: &SettlementId, author: &PeerId) -> Self {
        Self(format!("{}:{author}", settlement.as_str()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One to five stars.
///
/// An enum rather than a `u8` so a rating outside the scale cannot exist
/// in memory, cannot be stored, and cannot arrive from a peer: a record
/// claiming 200 stars fails to deserialize and never reaches the store,
/// instead of reaching it and relying on every later reader to
/// re-validate. It crosses the wire as the integer 1-5 (see the serde
/// impls below) because that is what an SDK in another language will
/// naturally send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rating {
    One,
    Two,
    Three,
    Four,
    Five,
}

impl Rating {
    pub const fn stars(self) -> u8 {
        match self {
            Self::One => 1,
            Self::Two => 2,
            Self::Three => 3,
            Self::Four => 4,
            Self::Five => 5,
        }
    }

    /// `None` for anything off the scale — the only way to build a
    /// `Rating` from untrusted input.
    pub const fn from_stars(stars: u8) -> Option<Self> {
        match stars {
            1 => Some(Self::One),
            2 => Some(Self::Two),
            3 => Some(Self::Three),
            4 => Some(Self::Four),
            5 => Some(Self::Five),
            _ => None,
        }
    }
}

impl serde::Serialize for Rating {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(self.stars())
    }
}

impl<'de> serde::Deserialize<'de> for Rating {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Stars;

        impl serde::de::Visitor<'_> for Stars {
            type Value = Rating;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a star rating from 1 to 5")
            }

            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Rating, E> {
                u8::try_from(value)
                    .ok()
                    .and_then(Rating::from_stars)
                    .ok_or_else(|| E::custom(format!("{value} is not a rating from 1 to 5")))
            }

            // JSON numbers arrive signed when written without a sign
            // hint by some encoders; a negative one is off the scale for
            // the same reason 200 is.
            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Rating, E> {
                u8::try_from(value)
                    .ok()
                    .and_then(Rating::from_stars)
                    .ok_or_else(|| E::custom(format!("{value} is not a rating from 1 to 5")))
            }
        }

        // Named explicitly rather than `deserialize_any`, because the
        // gossip wire format (postcard) is not self-describing and would
        // have nothing to answer `any` with.
        deserializer.deserialize_u8(Stars)
    }
}

/// Longest accepted [`Review::comment`], in characters.
///
/// What an unbounded field would cost, concretely: a review is gossiped
/// to every node, stored by every node forever, replayed to every node
/// that joins, and shipped inside every snapshot. At 500 characters one
/// review is at most 2 KB on the wire, so a network that settles a
/// million trades carries at most 4 GB of review text in perpetuity —
/// already the largest free-text field this protocol accepts, and the
/// reason the bound is a few hundred characters rather than a few
/// thousand. Unbounded, a single signed record is a denial-of-service
/// against every node's disk that costs its author one settlement and
/// cannot be withdrawn afterwards by anyone.
///
/// The same reasoning `openfiat_content::MAX_CAPTION_CHARS` and
/// `openfiat_identity`'s claim values apply, at the length a sentence
/// about a trade actually needs.
pub const MAX_COMMENT_CHARS: usize = 500;

/// One party's opinion of the other, after a trade they were both in.
///
/// # What this record does *not* contain
///
/// The wallet it is about. That is deliberate and it is the security of
/// the whole feature: a review states only which settlement it concerns,
/// and *who it is about is derived from the settlement* by
/// [`subject_of`]. A record cannot name its own subject, so it cannot
/// name a wallet that was never in the trade, and a reader that has the
/// review but not the settlement cannot attribute it to anybody at all.
///
/// Immutable in the sense that matters: the author may publish a later
/// review of the same trade, which supersedes their own earlier words
/// under a rule every node applies identically (see
/// [`crate::store::ReviewRegistry::apply_publish`]). Nobody can supersede
/// anybody else's.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Review {
    pub settlement: SettlementId,
    pub author: PeerId,
    pub author_public_key: PublicKey,
    pub rating: Rating,
    /// The author's own words. Rendered as text, never as markup — see
    /// [`MAX_COMMENT_CHARS`] for what bounds it and [`Review::validate`]
    /// for what it may not contain.
    ///
    /// Empty is legitimate: a star rating with nothing to add is a real
    /// review, and forcing prose out of someone produces filler.
    pub comment: String,
    pub created_at: Timestamp,
}

impl Review {
    /// Derived, never stored — see [`ReviewId`].
    pub fn id(&self) -> ReviewId {
        ReviewId::of(&self.settlement, &self.author)
    }

    /// The checks that need no state: shape, not authorization.
    ///
    /// Kept separate from the store's checks so a client can run it
    /// before signing, and so the reason a record was refused is either
    /// "this is malformed" or "you were not in this trade", never both at
    /// once.
    pub fn validate(&self) -> Result<(), ReviewError> {
        if self.comment.chars().count() > MAX_COMMENT_CHARS {
            return Err(ReviewError::MalformedReview);
        }
        if self.comment.chars().any(is_display_hazard) {
            return Err(ReviewError::MalformedReview);
        }
        Ok(())
    }
}

/// Characters a review may not contain, because of what they do to the
/// text *around* them rather than to themselves.
///
/// A review is rendered next to a wallet id, a star count and a date. A
/// bidirectional override reverses the characters that follow it, so a
/// comment can be made to read as though it were the label beside it or
/// as though its author were someone else; a carriage return or an escape
/// can redraw a terminal line, which is where a node operator reads these.
/// Newline survives because a paragraph break is ordinary prose.
fn is_display_hazard(c: char) -> bool {
    if c == '\n' {
        return false;
    }
    c.is_control() || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// A stored [`Review`] joined with the settlement that authorizes it, so
/// it can finally say who it is about.
///
/// This is the only shape a review is ever handed out in. A bare `Review`
/// is an unattributed claim; it becomes a review of somebody exactly when
/// a settlement record says the author was entitled to write it, and
/// nothing constructs this type without having gone through
/// [`subject_of`].
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PublishedReview {
    pub id: ReviewId,
    pub settlement: SettlementId,
    pub author: PeerId,
    /// The counterparty this review is about — read out of the
    /// settlement, never out of the review.
    pub about: PeerId,
    pub rating: Rating,
    pub comment: String,
    pub created_at: Timestamp,
}

impl PublishedReview {
    pub(crate) fn new(review: Review, about: PeerId) -> Self {
        Self {
            id: review.id(),
            settlement: review.settlement,
            author: review.author,
            about,
            rating: review.rating,
            comment: review.comment,
            created_at: review.created_at,
        }
    }
}

/// Whether a settlement has got far enough for its parties to have
/// anything to review, and therefore whether a review of it counts.
///
/// `Approved` as well as `Completed`, which is not a looseness. The step
/// from `Approved` to `Completed` is `SettlementRegistry::
/// apply_escrow_released` — local, unsigned bookkeeping performed by a
/// node that independently observed the on-chain release. A `GossipOnly`
/// node observes nothing on chain and so may hold a settlement at
/// `Approved` forever while an `RpcConnected` node holds the same one at
/// `Completed`. Gating on `Completed` alone would therefore make the same
/// review visible on some nodes and invisible on others: an authorization
/// rule that gives different answers per node is not one. Both states
/// mean the same thing to the two people involved — the merchant approved
/// the payment and the trade is over — and both are reached only through
/// signed events every node replicates identically.
pub const fn is_settled(state: SettlementState) -> bool {
    matches!(
        state,
        SettlementState::Approved | SettlementState::Completed
    )
}

/// Who `author` is entitled to review on `settlement`, if anyone.
///
/// This is the whole security of the feature, in one place so that no
/// caller can implement a slightly different version of it. `Some(other
/// party)` iff the settlement is settled and `author` is one of its two
/// parties; `None` otherwise — for a stranger, for a trade that was
/// cancelled, rejected or is still in flight, and for a disputed trade
/// whose outcome is not this crate's to summarise.
///
/// Note what it never consults: the review. Authorization is read out of
/// the settlement record, which is signed by both parties and replicated
/// to every node, and never out of what a review claims about itself.
pub fn subject_of(settlement: &Settlement, author: &PeerId) -> Option<PeerId> {
    if !is_settled(settlement.state) {
        return None;
    }
    if author == &settlement.buyer {
        return Some(settlement.seller.clone());
    }
    if author == &settlement.seller {
        return Some(settlement.buyer.clone());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{other, review, settled_settlement, settlement_in};

    #[test]
    fn a_rating_crosses_the_wire_as_a_plain_integer() {
        assert_eq!(serde_json::to_string(&Rating::Four).unwrap(), "4");
        assert_eq!(
            serde_json::from_str::<Rating>("4").unwrap(),
            Rating::Four,
            "an SDK in another language sends the number a user clicked"
        );
    }

    #[test]
    fn a_rating_off_the_scale_cannot_be_deserialized_at_all() {
        // The point of the enum: this fails here, rather than being
        // stored and then having to be re-checked by every reader.
        for hostile in ["0", "6", "255", "-1", "1000000"] {
            assert!(
                serde_json::from_str::<Rating>(hostile).is_err(),
                "{hostile} must not become a Rating"
            );
        }
    }

    #[test]
    fn a_comment_longer_than_the_bound_is_refused() {
        let huge = "a".repeat(MAX_COMMENT_CHARS + 1);
        assert_eq!(
            review("s-1", 1, Rating::Five, &huge).validate(),
            Err(ReviewError::MalformedReview)
        );
        assert_eq!(
            review("s-1", 1, Rating::Five, &"a".repeat(MAX_COMMENT_CHARS)).validate(),
            Ok(()),
            "the bound itself is acceptable"
        );
    }

    #[test]
    fn the_bound_counts_characters_rather_than_bytes() {
        // A four-byte character is one character to the person typing it,
        // and a byte bound would silently give non-Latin scripts a
        // quarter of the review length.
        let emoji = "😀".repeat(MAX_COMMENT_CHARS);
        assert_eq!(review("s-1", 1, Rating::Five, &emoji).validate(), Ok(()));
    }

    #[test]
    fn a_comment_that_could_redraw_the_text_around_it_is_refused() {
        for hazard in ["\u{202E}gnitaeh c", "done\r         5 stars", "a\u{0}b"] {
            assert_eq!(
                review("s-1", 1, Rating::Five, hazard).validate(),
                Err(ReviewError::MalformedReview),
                "{hazard:?} must not reach a renderer"
            );
        }
        assert_eq!(
            review("s-1", 1, Rating::Five, "fast\nand polite").validate(),
            Ok(()),
            "a paragraph break is ordinary prose"
        );
    }

    #[test]
    fn each_party_is_entitled_to_review_the_other_and_nobody_else() {
        let settlement = settled_settlement("s-1");
        assert_eq!(
            subject_of(&settlement, &settlement.buyer),
            Some(settlement.seller.clone()),
            "the buyer reviews the seller"
        );
        assert_eq!(
            subject_of(&settlement, &settlement.seller),
            Some(settlement.buyer.clone()),
            "and the seller reviews the buyer"
        );
        assert_eq!(
            subject_of(&settlement, &other()),
            None,
            "a wallet that was not in the trade is entitled to nothing"
        );
    }

    #[test]
    fn a_trade_that_did_not_settle_entitles_nobody_to_review_it() {
        for state in [
            SettlementState::AwaitingPayment,
            SettlementState::PaymentSubmitted,
            SettlementState::Rejected,
            SettlementState::Cancelled,
            SettlementState::Disputed,
        ] {
            let settlement = settlement_in(state);
            assert_eq!(
                subject_of(&settlement, &settlement.buyer),
                None,
                "{state:?} is not a completed trade"
            );
        }
    }

    /// A node that has seen the on-chain release and one that has not
    /// hold the same trade in different states, and must not disagree
    /// about whether its review counts.
    #[test]
    fn approval_and_completion_are_both_settled_because_nodes_differ_on_which_it_is() {
        for state in [SettlementState::Approved, SettlementState::Completed] {
            let settlement = settlement_in(state);
            assert!(subject_of(&settlement, &settlement.buyer).is_some());
        }
    }

    #[test]
    fn an_id_is_one_per_party_per_trade() {
        let settlement = settled_settlement("s-1");
        let buyers = ReviewId::of(&settlement.id, &settlement.buyer);
        assert_eq!(
            buyers,
            ReviewId::of(&settlement.id, &settlement.buyer),
            "the same party reviewing the same trade lands on the same key"
        );
        assert_ne!(buyers, ReviewId::of(&settlement.id, &settlement.seller));
        assert_ne!(
            buyers,
            ReviewId::of(&SettlementId::new("s-2"), &settlement.buyer)
        );
    }
}
