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
///
/// # Which types are here (as of F-01)
///
/// This originally covered only the signed payloads constructed and verified
/// entirely **inside this workspace** — by the node or the in-tree CLI, never
/// by an SDK or the app — because tagging those was a self-contained change
/// that could not desynchronise from an outside signer.
///
/// F-01 (2026-08) extends this to the full set of *client-signed* wire
/// types — everything the SDKs (`openfiat-sdks`) or the app sign, which is
/// most of the marketplace. Their preimage is a cross-repo contract: the
/// Rust SDK, the TypeScript SDK, and the app each reconstruct it
/// independently, so the tag literals below are the frozen source of truth,
/// and `tests/vectors/client_signed_v1.json` (checked by
/// `tests/conformance_vectors.rs`) is the byte-for-byte proof that all four
/// surfaces agree on the header. Landing the tag here is only step one —
/// each node crate's own `Signed*::sign()`/`verify()` still has to switch to
/// `preimage(tag::X, &payload)` (tracked per crate in the F-01 program) and
/// the TS SDK / app need the equivalent header helper before the tag is
/// actually load-bearing for that type.
///
/// The structural guard in `tests/signed_payload_shapes.rs` protects every
/// type that has not yet made that switch: it fails the build the moment any
/// two signed payloads share a field shape, which is the only condition
/// under which a missing tag becomes exploitable. It also enumerates every
/// `Signed*` type known at F-01 time, so a newly added one that is not on
/// that list — and therefore was never triaged for a tag — fails the build
/// too.
pub mod tag {
    pub const CLAIM_VERIFY: &str = "openfiat/identity/ClaimVerify/v1";
    pub const CLAIM_REVOKE: &str = "openfiat/identity/ClaimRevoke/v1";
    pub const PROPOSAL_WITHDRAW: &str = "openfiat/governance/ProposalWithdraw/v1";
    pub const PROPOSAL_ACTIVATE: &str = "openfiat/governance/ProposalActivate/v1";
    pub const MUTUAL_SETTLEMENT_AGREE: &str = "openfiat/disputes/MutualSettlementAgree/v1";
    pub const PAYMENT_METHOD_DEFINE: &str = "openfiat/taxonomy/PaymentMethodDefine/v1";
    pub const FEE_SETTLEMENT: &str = "openfiat/registry/FeeSettlement/v1";

    // --- F-01: client-signed wire types (2026-08-08) ---------------------
    // Every tag below is a cross-repo contract: the literal must be
    // byte-identical to its counterpart in the TS SDK's `tags.ts` and the
    // app's tag table. Do not rename one without renaming all three in the
    // same release, and never reuse a literal for a different type.

    // advertisements
    pub const ADVERTISEMENT_CREATE: &str = "openfiat/advertisements/AdvertisementCreate/v1";
    pub const ADVERTISEMENT_STATUS_SET: &str = "openfiat/advertisements/AdvertisementStatusSet/v1";
    pub const ADVERTISEMENT_TERMS_UPDATE: &str =
        "openfiat/advertisements/AdvertisementTermsUpdate/v1";
    pub const ADVERTISEMENT_PRICE_UPDATE: &str =
        "openfiat/advertisements/AdvertisementPriceUpdate/v1";

    // reservations
    pub const RESERVATION_REQUEST: &str = "openfiat/reservations/ReservationRequest/v1";
    pub const RESERVATION_CANCEL: &str = "openfiat/reservations/ReservationCancel/v1";

    // settlement
    pub const SETTLEMENT_INITIATE: &str = "openfiat/settlement/SettlementInitiate/v1";
    pub const PAYMENT_SUBMITTED: &str = "openfiat/settlement/PaymentSubmitted/v1";
    pub const PAYMENT_REVERSED: &str = "openfiat/settlement/PaymentReversed/v1";
    pub const SETTLEMENT_APPROVED: &str = "openfiat/settlement/SettlementApproved/v1";
    pub const SETTLEMENT_REJECTED: &str = "openfiat/settlement/SettlementRejected/v1";
    pub const SETTLEMENT_CANCELLED: &str = "openfiat/settlement/SettlementCancelled/v1";

    // registry (FeeSettlement above is already tagged)
    pub const REGISTRATION: &str = "openfiat/registry/Registration/v1";
    pub const HEALTH_UPDATE: &str = "openfiat/registry/HealthUpdate/v1";
    pub const WITHDRAWAL: &str = "openfiat/registry/Withdrawal/v1";

    // sessions
    pub const SESSION_CREATE: &str = "openfiat/sessions/SessionCreate/v1";
    pub const SESSION_RENEW: &str = "openfiat/sessions/SessionRenew/v1";
    pub const SESSION_REVOKE: &str = "openfiat/sessions/SessionRevoke/v1";
    pub const SESSION_MIGRATE: &str = "openfiat/sessions/SessionMigrate/v1";

    // reviews
    pub const REVIEW_PUBLISH: &str = "openfiat/reviews/ReviewPublish/v1";

    // risk
    pub const RISK_PUBLISH: &str = "openfiat/risk/RiskPublish/v1";

    // oracles
    pub const ORACLE_PUBLISH: &str = "openfiat/oracles/OraclePublish/v1";

    // snapshot (node-signed today, but tagged too — see the module doc)
    pub const SNAPSHOT_ANNOUNCE: &str = "openfiat/snapshot/SnapshotAnnounce/v1";

    // disputes (MutualSettlementAgree above is already tagged)
    pub const DISPUTE_OPEN: &str = "openfiat/disputes/DisputeOpen/v1";
    pub const ARBITRATOR_JOIN: &str = "openfiat/disputes/ArbitratorJoin/v1";
    pub const DISPUTE_VOTE_COMMIT: &str = "openfiat/disputes/VoteCommit/v1";
    pub const DISPUTE_VOTE_REVEAL: &str = "openfiat/disputes/VoteReveal/v1";

    // governance (ProposalWithdraw/ProposalActivate above are already tagged)
    pub const PROPOSAL_CREATE: &str = "openfiat/governance/ProposalCreate/v1";
    pub const VOTE_CAST: &str = "openfiat/governance/VoteCast/v1";

    // identity (ClaimVerify/ClaimRevoke above are already tagged)
    pub const CLAIM_PUBLISH: &str = "openfiat/identity/ClaimPublish/v1";

    // content (evidence attachments — the design doc's "AttachmentPublish")
    pub const ATTACHMENT_PUBLISH: &str = "openfiat/content/AttachmentPublish/v1";

    // notifications
    pub const SUBSCRIPTION_UPDATE: &str = "openfiat/notifications/SubscriptionUpdate/v1";
    pub const DELIVERY_REPORT: &str = "openfiat/notifications/DeliveryReport/v1";

    // tradechannel
    pub const TRADE_CHANNEL_KEY_GRANT: &str = "openfiat/tradechannel/TradeChannelKeyGrant/v1";
    pub const TRADE_CHANNEL_ENTRY_POST: &str = "openfiat/tradechannel/TradeChannelEntryPost/v1";
}

/// The bytes to sign for `payload` under `tag`.
///
/// Layout: `len(tag)` as a 4-byte big-endian integer, the tag's UTF-8 bytes,
/// then the payload's JSON. See the module doc for why the length prefix is
/// not decoration.
pub fn preimage<T: Serialize>(tag: &str, payload: &T) -> Result<Vec<u8>, EncodeError> {
    let body = json::to_bytes(payload)?;
    Ok(preimage_raw(tag, &body))
}

/// The payload-agnostic form of [`preimage`]: builds the same
/// `len(tag):u32be ‖ tag ‖ body` header around an already-encoded body.
///
/// This is what makes the header a testable, language-independent contract:
/// a conformance vector can supply `body` as opaque bytes (e.g. a JSON
/// string produced by a *different* language's encoder) and assert the
/// resulting preimage matches, without this crate ever deserializing or
/// caring what `body` contains.
pub fn preimage_raw(tag: &str, body: &[u8]) -> Vec<u8> {
    let tag_bytes = tag.as_bytes();
    let mut out = Vec::with_capacity(4 + tag_bytes.len() + body.len());
    out.extend_from_slice(&(tag_bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(tag_bytes);
    out.extend_from_slice(body);
    out
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
