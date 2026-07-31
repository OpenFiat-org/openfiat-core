//! `openfiat-reviews` — what one counterparty says about the other once a
//! trade is over.
//!
//! # Why this is not part of `openfiat-reputation`
//!
//! Because they are different kinds of statement, and the difference is
//! the reason both are trustworthy.
//!
//! A reputation profile is *evidence*: `openfiat-reputation` computes it
//! by re-reading settlements, reservations and disputes that every node
//! already replicates, so every node derives the same numbers from the
//! same signed events and nobody asserts anything about themselves. That
//! crate deliberately has no signed event and no store of its own —
//! OFS-3000 §26 requires that "only cryptographically verified protocol
//! events SHALL modify reputation".
//!
//! A review is an *opinion*. Its signature proves who wrote it and
//! nothing whatever about whether it is true. Folding one into the
//! computed score would mean the score no longer measures what happened,
//! and it would be trivially bought: two wallets can trade with each
//! other repeatedly and five-star each other every time, at the cost of
//! their own escrow round-trips, and the number every merchant is ranked
//! by would follow. So a review never touches [`ReputationProfile`] —
//! there is no field on that struct for one and this crate cannot reach
//! it — and a client shows the two side by side: the trade record, and
//! what people said.
//!
//! [`ReputationProfile`]: https://docs.rs/openfiat-reputation
//!
//! Keeping them in separate crates is what makes that structural rather
//! than a promise: `openfiat-reputation` does not depend on this crate,
//! so no review can reach a score even by accident.
//!
//! # What makes a review trustworthy at all
//!
//! Exactly one thing: only a counterparty of a real, settled trade may
//! write one, only about the other party, and only once per trade. Take
//! that away and the reputation surface becomes a place where strangers
//! write about people they have never met. It is enforced in
//! [`record::subject_of`], out of the settlement record — which both
//! parties signed and every node holds — and never out of anything the
//! review says about itself. A review does not even carry the wallet it
//! is about; that is derived from the trade.
//!
//! See [`store`] for why that check runs on the read path rather than the
//! write path (gossip has no ordering guarantee, and a review can arrive
//! before the trade it reviews), and [`view`] for who may read what, which
//! is the other half of the design: a review names two people, and this
//! network deliberately does not publish who trades with whom.

pub mod error;
pub mod events;
pub mod protocol;
pub mod record;
pub mod store;
pub mod view;

#[cfg(test)]
mod fixtures;

pub use error::ReviewError;
pub use events::SignedReviewPublish;
pub use record::{
    MAX_COMMENT_CHARS, PublishedReview, Rating, Review, ReviewId, is_settled, subject_of,
};
pub use store::{COLUMN_FAMILY as REVIEWS_COLUMN_FAMILY, ReviewRegistry};
pub use view::{PublicReview, ReviewsView};

/// Crate version, re-exported for diagnostics and `openfiat-node --version`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_a_version() {
        assert!(!version().is_empty());
    }
}
