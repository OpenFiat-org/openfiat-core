//! The commit-reveal commitment scheme (Ch.11 §11.12, OFS-2400 §16):
//! "each arbitrator submits a cryptographic commitment during the commit
//! phase, then reveals the vote and secret during the reveal phase. Only
//! revealed votes matching their earlier commitment are counted."

use crate::record::Vote;
use openfiat_crypto::sha256;

/// The commitment an arbitrator publishes during the commit phase:
/// `sha256(vote || secret)`. `secret` should be a fresh random 32-byte
/// value the arbitrator keeps hidden until the reveal phase — reusing a
/// secret, or one predictable from the vote alone, would let other
/// arbitrators or observers infer the vote before it's revealed.
pub fn compute(vote: Vote, secret: &[u8; 32]) -> [u8; 32] {
    let vote_byte = match vote {
        Vote::BuyerWins => 0u8,
        Vote::MerchantWins => 1u8,
        Vote::Invalid => 2u8,
    };
    let mut input = Vec::with_capacity(33);
    input.push(vote_byte);
    input.extend_from_slice(secret);
    sha256(&input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_when_recomputed_with_the_same_inputs() {
        let secret = [7u8; 32];
        assert_eq!(compute(Vote::BuyerWins, &secret), compute(Vote::BuyerWins, &secret));
    }

    #[test]
    fn differs_for_a_different_vote_with_the_same_secret() {
        let secret = [7u8; 32];
        assert_ne!(compute(Vote::BuyerWins, &secret), compute(Vote::MerchantWins, &secret));
    }

    #[test]
    fn differs_for_a_different_secret_with_the_same_vote() {
        assert_ne!(compute(Vote::BuyerWins, &[1u8; 32]), compute(Vote::BuyerWins, &[2u8; 32]));
    }
}
