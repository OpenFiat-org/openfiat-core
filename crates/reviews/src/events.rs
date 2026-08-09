//! The one signed review event.
//!
//! Self-consistency verified, the same shape `openfiat_identity`'s
//! `SignedClaimPublish` uses: the record carries the key it was signed
//! with, that key must derive to the `author` the record names (OFNP §6),
//! and the signature must verify against it. What this establishes is
//! only *who wrote it* — never that they were entitled to write it, which
//! needs the settlement and lives in [`crate::record::subject_of`].

use crate::error::ReviewError;
use crate::record::Review;
use openfiat_crypto::{Keypair, verify};
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_types::Signature;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedReviewPublish {
    pub review: Review,
    pub signature: Signature,
}

impl SignedReviewPublish {
    pub fn sign(review: Review, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::REVIEW_PUBLISH,
            &review,
        )
        .expect("a Review always serializes");
        Self {
            signature: keypair.sign(&bytes),
            review,
        }
    }

    pub fn verify(&self) -> Result<(), ReviewError> {
        let expected = peer_id_from_public_key(&self.review.author_public_key)
            .map_err(|_| ReviewError::InvalidSignature)?;
        // A record whose key does not derive to the author it names is
        // refused before the signature is even checked: a valid signature
        // by a key that is not the claimed author's proves nothing about
        // the author.
        if expected != self.review.author {
            return Err(ReviewError::InvalidSignature);
        }
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::REVIEW_PUBLISH,
            &self.review,
        )
        .map_err(|_| ReviewError::MalformedReview)?;
        verify(&self.review.author_public_key, &bytes, &self.signature)
            .map_err(|_| ReviewError::InvalidSignature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{keypair, review};
    use crate::record::Rating;

    #[test]
    fn a_review_signed_by_its_author_verifies() {
        let signed =
            SignedReviewPublish::sign(review("s-1", 1, Rating::Five, "quick"), &keypair(1));
        assert_eq!(signed.verify(), Ok(()));
    }

    #[test]
    fn a_review_signed_by_anyone_else_does_not() {
        // The impostor signs correctly — with their own key, over a
        // record naming somebody else as the author.
        let signed = SignedReviewPublish::sign(review("s-1", 1, Rating::One, "awful"), &keypair(2));
        assert_eq!(signed.verify(), Err(ReviewError::InvalidSignature));
    }

    #[test]
    fn changing_a_word_after_signing_invalidates_it() {
        let mut signed =
            SignedReviewPublish::sign(review("s-1", 1, Rating::Five, "polite"), &keypair(1));
        signed.review.comment = "rude".to_string();
        assert_eq!(signed.verify(), Err(ReviewError::InvalidSignature));
    }

    /// Naming a key that is genuinely yours while claiming to be a
    /// different wallet is the one combination a naive signature check
    /// would let through.
    #[test]
    fn a_key_that_does_not_derive_to_the_named_author_is_refused() {
        let mut review = review("s-1", 1, Rating::Five, "fine");
        review.author_public_key = keypair(2).public_key();
        let signed = SignedReviewPublish::sign(review, &keypair(2));
        assert_eq!(signed.verify(), Err(ReviewError::InvalidSignature));
    }
}
