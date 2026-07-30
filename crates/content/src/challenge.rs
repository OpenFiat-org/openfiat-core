//! Proving that a node actually holds content the protocol references.
//!
//! # What a challenge is
//!
//! One node picks a CID the network knows about, asks another node for
//! the bytes, and hashes what comes back. A content address *is* the hash
//! of its content, so returning the right bytes is not something a node
//! can do without having them. That makes this the one node-quality
//! signal in this workspace that is checked rather than believed —
//! contrast `openfiat_rewards::liveness`, where connectivity is a claim
//! the ledger takes at face value because nothing available can test it.
//!
//! # Only some CIDs can be challenged, and the reason is not arbitrary
//!
//! A raw-codec CID's digest is taken over the file itself, so hashing the
//! response decides the question outright. A dag-pb CID addresses the
//! root node of a chunked DAG, and its digest covers that node's encoding
//! rather than the file — so a peer could return the correct file and
//! still fail a naive hash check.
//!
//! Providers switch between the two at 262,144 bytes. That is IPFS's
//! standard chunk size rather than one provider's quirk, and it was
//! measured rather than assumed: uploading 262,144 bytes to Filebase
//! returns a raw CID and 262,145 returns a dag-pb root, with `raw-leaves`
//! and `chunker` both ignored.
//!
//! So [`challengeable`] filters to what can be decided, and the rest of
//! this module cannot be handed anything else. The alternative — checking
//! every CID and treating an unverifiable one as a failure — would punish
//! honest nodes for holding large files, which is the opposite of what
//! the reward is for.
//!
//! # What it therefore samples
//!
//! Files at or under 256 KiB: avatars almost always, attachments
//! sometimes. A node that pins everything answers these; a node that pins
//! nothing cannot answer any. That is enough to separate the two, which
//! is what the reward multiplier needs. It is not enough to prove a node
//! holds one *specific* large attachment, and nothing here claims it is.

use crate::record::Attachment;
use openfiat_crypto::Cid;

/// The CIDs among `attachments` that a challenge can actually decide.
///
/// Deduplicated and ordered, so two nodes given the same attachments draw
/// from the same list — a challenger that picked from an arbitrarily
/// ordered set would produce results that varied by iteration order
/// rather than by what the challenged node holds.
pub fn challengeable(attachments: &[Attachment], now: openfiat_types::Timestamp) -> Vec<Cid> {
    let mut cids: Vec<Cid> = attachments
        .iter()
        // Inside the protocol's retention floor, not the caller's own
        // window. A node that correctly evicted year-old content under a
        // rolling policy must not be challenged about it — see
        // `crate::retention` for why the floor is what both sides agree
        // on, and why it is not something either can configure away.
        .filter(|a| a.cid.is_verifiable() && crate::Retention::challenge_window(a.created_at, now))
        .map(|a| a.cid.clone())
        .collect();
    cids.sort();
    cids.dedup();
    cids
}

/// Picks which CID to challenge for, from `seed`.
///
/// `seed` should vary per challenge — an epoch slice, say — so a node
/// cannot hold one lucky file and pass forever. Returns `None` when there
/// is nothing verifiable to ask about, which is a real state on a young
/// network and must not be mistaken for a node that failed.
pub fn select(cids: &[Cid], seed: u64) -> Option<&Cid> {
    if cids.is_empty() {
        return None;
    }
    cids.get((seed % cids.len() as u64) as usize)
}

/// Whether `response` is the content `cid` names.
///
/// The whole check. Nothing about the responding node is trusted: not its
/// identity, not its claims, not how quickly it answered. Either the bytes
/// hash to the digest or they do not.
///
/// Takes a `&Cid` that callers are expected to have taken from
/// [`challengeable`]; an unverifiable CID returns `false` here, which is
/// correct in isolation but would be a wrong *conclusion* about the peer,
/// so [`ChallengeOutcome`] exists to keep the two apart.
pub fn verify(cid: &Cid, response: &[u8]) -> bool {
    cid.is_verifiable() && cid.matches(response)
}

/// What a challenge established.
///
/// Three outcomes and not two: "the peer failed" and "this challenge
/// could not decide anything" have to stay distinguishable, or a node
/// gets penalised for our inability to check rather than for its own
/// behaviour. Only [`ChallengeOutcome::Served`] should reach
/// `LivenessLedger::observe_content_served`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChallengeOutcome {
    /// Returned bytes matching the CID. Proven.
    Served,
    /// Answered with something else, or did not answer.
    Failed,
    /// The CID could not be checked by this code — see the module doc.
    /// Says nothing at all about the peer.
    Undecidable,
}

/// Judges one response.
pub fn judge(cid: &Cid, response: Option<&[u8]>) -> ChallengeOutcome {
    if !cid.is_verifiable() {
        return ChallengeOutcome::Undecidable;
    }
    match response {
        Some(bytes) if cid.matches(bytes) => ChallengeOutcome::Served,
        _ => ChallengeOutcome::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;
    use crate::record::{AttachmentId, AttachmentSubject, MediaType};
    use openfiat_settlement::SettlementId;
    use openfiat_types::{PeerId, PublicKey, Timestamp};

    /// The bytes `fixtures::PROBE_CID` actually names — uploaded to
    /// Filebase, retrieved from ipfs.io, and reproduced byte for byte by
    /// an independent CID implementation.
    const PROBE_CONTENT: &[u8] = b"openfiat ipfs probe 1785426891\n";

    /// A real dag-pb root, from a 900 KB upload to the same provider.
    const CHUNKED_CID: &str = "bafybeig3ci7io2duyknu34co3zw42oodnfyamwazsus42vpgnvq2hpzodm";

    fn attachment(id: &str, cid: openfiat_crypto::Cid) -> Attachment {
        Attachment {
            id: AttachmentId::new(id),
            subject: AttachmentSubject::Settlement(SettlementId::new("s-1")),
            author: PeerId::from_bytes(vec![1; 8]),
            author_public_key: PublicKey::from_bytes([2; 32]),
            cid,
            media_type: MediaType::Png,
            size_bytes: 31,
            caption: String::new(),
            created_at: NOW,
        }
    }

    /// Fixture attachments are stamped "now", so they sit inside the
    /// retention floor and the tests below isolate the property they are
    /// actually about.
    const NOW: Timestamp = Timestamp::from_millis(20_000 * 24 * 60 * 60 * 1_000);

    #[test]
    fn returning_the_named_content_is_a_proof() {
        let cid = fixtures::probe_cid();
        assert!(verify(&cid, PROBE_CONTENT));
        assert_eq!(judge(&cid, Some(PROBE_CONTENT)), ChallengeOutcome::Served);
    }

    #[test]
    fn returning_anything_else_fails() {
        let cid = fixtures::probe_cid();
        for wrong in [
            b"".as_slice(),
            b"openfiat ipfs probe 1785426891".as_slice(), // one byte short
            b"something else entirely".as_slice(),
        ] {
            assert!(!verify(&cid, wrong));
            assert_eq!(judge(&cid, Some(wrong)), ChallengeOutcome::Failed);
        }
    }

    #[test]
    fn no_answer_is_a_failure() {
        assert_eq!(
            judge(&fixtures::probe_cid(), None),
            ChallengeOutcome::Failed
        );
    }

    #[test]
    fn a_chunked_cid_is_undecidable_rather_than_a_failure() {
        // The distinction that keeps an honest node holding a large file
        // from being scored as if it had refused to answer.
        let chunked = openfiat_crypto::Cid::parse(CHUNKED_CID).unwrap();
        assert!(!chunked.is_verifiable());
        assert_eq!(
            judge(&chunked, Some(b"the genuine file")),
            ChallengeOutcome::Undecidable
        );
        assert_ne!(judge(&chunked, None), ChallengeOutcome::Served);
    }

    #[test]
    fn only_decidable_cids_are_offered_for_challenge() {
        let chunked = openfiat_crypto::Cid::parse(CHUNKED_CID).unwrap();
        let pool = challengeable(
            &[
                attachment("a", fixtures::probe_cid()),
                attachment("b", chunked),
                attachment("c", fixtures::other_cid()),
            ],
            NOW,
        );
        assert_eq!(pool.len(), 2, "the dag-pb CID must not be offered");
        assert!(pool.iter().all(|c| c.is_verifiable()));
    }

    #[test]
    fn the_same_attachments_always_produce_the_same_pool() {
        // Two nodes challenging from differently-ordered inputs must draw
        // from the same list, or a result depends on iteration order
        // rather than on what the peer holds.
        let forward = challengeable(
            &[
                attachment("a", fixtures::probe_cid()),
                attachment("b", fixtures::other_cid()),
            ],
            NOW,
        );
        let backward = challengeable(
            &[
                attachment("b", fixtures::other_cid()),
                attachment("a", fixtures::probe_cid()),
            ],
            NOW,
        );
        assert_eq!(forward, backward);
    }

    #[test]
    fn one_cid_referenced_twice_is_offered_once() {
        let pool = challengeable(
            &[
                attachment("a", fixtures::probe_cid()),
                attachment("b", fixtures::probe_cid()),
            ],
            NOW,
        );
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn selection_moves_with_the_seed_and_stays_in_range() {
        let pool = challengeable(
            &[
                attachment("a", fixtures::probe_cid()),
                attachment("b", fixtures::other_cid()),
            ],
            NOW,
        );
        let picks: Vec<_> = (0..8).map(|seed| select(&pool, seed).unwrap()).collect();
        assert!(
            picks.iter().any(|c| *c != picks[0]),
            "a fixed pick would let one lucky file pass forever"
        );
        assert!(picks.iter().all(|c| pool.contains(c)));
    }

    #[test]
    fn content_older_than_the_retention_floor_is_never_challenged() {
        // The property that stops eviction and rewards contradicting each
        // other. A rolling node that dropped this content did what it was
        // configured to do, and must not be scored as if it refused.
        const DAY: u64 = 24 * 60 * 60 * 1_000;
        let mut old = attachment("ancient", fixtures::probe_cid());
        old.created_at = Timestamp::from_millis(NOW.as_millis() - 31 * DAY);
        let mut recent = attachment("recent", fixtures::other_cid());
        recent.created_at = Timestamp::from_millis(NOW.as_millis() - 29 * DAY);

        let pool = challengeable(&[old, recent], NOW);
        assert_eq!(pool, vec![fixtures::other_cid()]);
    }

    #[test]
    fn an_empty_pool_selects_nothing_rather_than_panicking() {
        // A young network has no attachments at all. That is not a peer
        // failing a challenge, and it must not be reachable as one.
        assert!(select(&[], 7).is_none());
        assert!(select(&challengeable(&[], NOW), 0).is_none());
    }
}
