//! Domain-separated signing preimages.
//!
//! # The failure this exists to stop
//!
//! Every signed event in this workspace is signed over
//! `serde_json::to_vec(&payload)` and nothing else. Two payload types whose
//! fields have the same *names* and *types* therefore produce byte-identical
//! preimages for the same values, and a signature made for one is a valid
//! signature for the other. That is not hypothetical: `identity::ClaimVerify`
//! and `identity::ClaimRevoke` are both `{claim_id, wallet, timestamp}`, and
//! `apply_verify`/`apply_revoke` accept the same preconditions — so a
//! `SignedClaimVerify`, which is gossiped in the clear, could be lifted by any
//! observer and replayed as a permanent revocation of the claim it verified.
//! `governance::ProposalWithdraw` and `governance::ProposalActivate` collide
//! the same way.
//!
//! The signature was never wrong; it was answering a question that did not
//! include "…as what?".
//!
//! # The fix
//!
//! Bind the payload's type into the bytes that get signed. A signature then
//! attests to a *typed* statement, and re-labelling it is a different message
//! that the same signature does not cover.
//!
//! # Why this shape
//!
//! The tag is length-prefixed rather than merely concatenated. With a bare
//! `tag || json` join, a tag ending in the prefix of another plus a payload
//! whose first bytes make up the difference could still collide — the classic
//! concatenation ambiguity. A length prefix makes the parse unambiguous, so
//! distinct `(tag, payload)` pairs always give distinct preimages.
//!
//! # What this is not
//!
//! Not a replacement for a nonce or an expiry. A domain tag stops a signature
//! meaning something it never meant; it does not stop the *same* statement
//! being replayed later, nor does it separate one network from another. Both
//! remain open and are tracked separately.

use serde::Serialize;

use crate::json::{self, EncodeError};

/// Domain tags. One per signed payload type — never share a tag between two
/// types, since sharing one recreates exactly the collision this prevents.
pub mod tag {
    pub const CLAIM_VERIFY: &str = "openfiat/identity/ClaimVerify/v1";
    pub const CLAIM_REVOKE: &str = "openfiat/identity/ClaimRevoke/v1";
    pub const PROPOSAL_WITHDRAW: &str = "openfiat/governance/ProposalWithdraw/v1";
    pub const PROPOSAL_ACTIVATE: &str = "openfiat/governance/ProposalActivate/v1";
}

/// The bytes to sign for `payload` under `tag`.
///
/// Layout: `len(tag)` as a 4-byte big-endian integer, the tag's UTF-8 bytes,
/// then the payload's JSON. See the module doc for why the length prefix is
/// not decoration.
pub fn preimage<T: Serialize>(tag: &str, payload: &T) -> Result<Vec<u8>, EncodeError> {
    let body = json::to_bytes(payload)?;
    let tag_bytes = tag.as_bytes();
    let mut out = Vec::with_capacity(4 + tag_bytes.len() + body.len());
    out.extend_from_slice(&(tag_bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(tag_bytes);
    out.extend_from_slice(&body);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Serialize)]
    struct Payload {
        a: u64,
        b: String,
    }

    fn payload() -> Payload {
        Payload {
            a: 7,
            b: "x".to_string(),
        }
    }

    #[test]
    fn the_same_payload_under_two_tags_signs_different_bytes() {
        // The whole point: this is what stops a signature for one event type
        // being valid for another with the same fields.
        assert_ne!(
            preimage(tag::CLAIM_VERIFY, &payload()).unwrap(),
            preimage(tag::CLAIM_REVOKE, &payload()).unwrap()
        );
    }

    #[test]
    fn the_same_payload_under_the_same_tag_is_stable() {
        // Signing is only reproducible if this is deterministic.
        assert_eq!(
            preimage(tag::CLAIM_VERIFY, &payload()).unwrap(),
            preimage(tag::CLAIM_VERIFY, &payload()).unwrap()
        );
    }

    #[test]
    fn the_payload_is_still_recoverable_json_after_the_prefix() {
        // The tag is a prefix, not an envelope — anything that already reads
        // these bytes as JSON keeps working once it skips the header.
        let bytes = preimage(tag::CLAIM_VERIFY, &payload()).unwrap();
        let tag_len = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
        assert_eq!(&bytes[4..4 + tag_len], tag::CLAIM_VERIFY.as_bytes());
        assert_eq!(
            &bytes[4 + tag_len..],
            &json::to_bytes(&payload()).unwrap()[..]
        );
    }

    #[test]
    fn a_tag_boundary_cannot_be_forged_by_shifting_bytes_into_the_payload() {
        // Without the length prefix, `"ab" || "cX"` and `"abc" || "X"` are the
        // same byte string and the separation is worthless. With it they differ.
        #[derive(serde::Serialize)]
        struct Raw(String);
        let left = preimage("ab", &Raw("cX".to_string())).unwrap();
        let right = preimage("abc", &Raw("X".to_string())).unwrap();
        assert_ne!(left, right);
    }
}
