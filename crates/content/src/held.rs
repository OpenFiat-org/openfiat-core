//! The content this node keeps a copy of, so it can answer a challenge.
//!
//! # Why a node keeps its own copy at all
//!
//! A challenge has to be answered from a synchronous RPC handler — this
//! workspace's dispatch is synchronous end to end — and asking an IPFS
//! daemon for bytes is not synchronous. So the node pins through the
//! daemon (which is what makes the content available to the wider IPFS
//! network) and separately keeps the bytes here, where a handler can
//! reach them without awaiting anything.
//!
//! That sounds like storing everything twice, and it would be, were it
//! not for the bound below.
//!
//! # The bound is the same fact that makes verification possible
//!
//! Only a raw-codec CID can be checked by hashing, and providers emit raw
//! CIDs only at or under 256 KiB — above that the content is chunked and
//! the CID addresses a DAG node instead. So the content worth holding for
//! challenges is exactly the content that is small.
//!
//! One constraint, doing two jobs: the set this node must keep is capped
//! at 256 KiB per item *because* that is the set a challenger can decide.
//! A large attachment is pinned through the daemon like any other and
//! simply never becomes a challenge, which costs nothing — a node that
//! answers the small ones has already demonstrated it is pinning.

use openfiat_crypto::Cid;
use openfiat_storage::KvStore;

const COLUMN_FAMILY: &str = "pinned_content";

/// The largest single item this node will hold locally.
///
/// IPFS's standard chunk size, and therefore the exact ceiling on a
/// raw-codec CID. Measured against a real provider: 262,144 bytes returns
/// a raw CID, 262,145 returns a dag-pb root.
pub const MAX_HELD_BYTES: usize = 262_144;

pub struct HeldContent<S> {
    store: S,
}

impl<S: KvStore> HeldContent<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }

    /// Stores `content` under `cid`, if it genuinely is that content.
    ///
    /// Returns whether it was kept. Refuses silently rather than erroring
    /// on the two cases that are ordinary rather than exceptional: content
    /// too large to be challengeable, and a CID whose codec cannot be
    /// checked. Both mean "not challenge material", not "something went
    /// wrong".
    pub fn keep(&self, cid: &Cid, content: &[u8]) -> bool {
        if content.len() > MAX_HELD_BYTES || !cid.is_verifiable() {
            return false;
        }
        // The last line of defence before this node starts serving these
        // bytes to challengers as though they were the named content.
        if !cid.matches(content) {
            return false;
        }
        self.store
            .put(COLUMN_FAMILY, cid.as_str().as_bytes(), content)
            .is_ok()
    }

    /// The content this node holds for `cid`, if any.
    pub fn get(&self, cid: &Cid) -> Option<Vec<u8>> {
        self.store
            .get(COLUMN_FAMILY, cid.as_str().as_bytes())
            .ok()
            .flatten()
    }

    pub fn holds(&self, cid: &Cid) -> bool {
        self.get(cid).is_some()
    }

    /// How many items are held — for `getNodeInfo`-style reporting, so an
    /// operator can see their node is actually doing the thing it is
    /// being paid a premium for.
    pub fn count(&self) -> usize {
        self.store
            .iter_prefix(COLUMN_FAMILY, &[])
            .map(|entries| entries.len())
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;
    use openfiat_storage::mem::MemoryStore;

    const PROBE_CONTENT: &[u8] = b"openfiat ipfs probe 1785426891\n";

    fn held() -> HeldContent<MemoryStore> {
        HeldContent::new(MemoryStore::new())
    }

    #[test]
    fn keeps_and_returns_content_that_matches_its_cid() {
        let store = held();
        let cid = fixtures::probe_cid();
        assert!(store.keep(&cid, PROBE_CONTENT));
        assert_eq!(store.get(&cid).as_deref(), Some(PROBE_CONTENT));
        assert!(store.holds(&cid));
        assert_eq!(store.count(), 1);
    }

    #[test]
    fn refuses_content_that_is_not_what_the_cid_names() {
        // Otherwise this node would serve a challenger bytes it cannot
        // stand behind, and fail a challenge it should have passed.
        let store = held();
        let cid = fixtures::probe_cid();
        assert!(!store.keep(&cid, b"not the probe content"));
        assert!(!store.holds(&cid));
    }

    #[test]
    fn refuses_content_past_the_chunk_boundary() {
        // 256 KiB + 1: above this a provider would have produced a dag-pb
        // CID, so content this large under a raw CID is malformed input.
        let store = held();
        let oversized = vec![0u8; MAX_HELD_BYTES + 1];
        assert!(!store.keep(&fixtures::probe_cid(), &oversized));
    }

    #[test]
    fn refuses_a_cid_no_challenge_could_decide() {
        let chunked =
            Cid::parse("bafybeig3ci7io2duyknu34co3zw42oodnfyamwazsus42vpgnvq2hpzodm").unwrap();
        let store = held();
        assert!(!store.keep(&chunked, b"anything"));
        assert!(!store.holds(&chunked));
    }

    #[test]
    fn content_never_kept_is_absent_rather_than_empty() {
        // A caller must be able to tell "I don't have this" from "I have
        // this and it is zero bytes", since only the first means the
        // challenge should be answered by someone else.
        let store = held();
        assert_eq!(store.get(&fixtures::other_cid()), None);
        assert_eq!(store.count(), 0);
    }
}
