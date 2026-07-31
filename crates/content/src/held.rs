//! The blocks this node keeps a copy of, so it can serve and be checked.
//!
//! # Why a node keeps its own copy at all
//!
//! A challenge has to be answered from a synchronous RPC handler — this
//! workspace's dispatch is synchronous end to end — and asking an IPFS
//! daemon for bytes is not synchronous. So the bytes live here, where a
//! handler can reach them without awaiting anything, and bitswap serves
//! them straight out of the same place.
//!
//! # A block is the unit, which is what lets a large file be held at all
//!
//! This store once held whole files under 256 KiB and refused everything
//! above, because only a raw-codec CID's digest covers the file bytes and
//! a larger file arrives as a dag-pb root that no hash check can decide.
//! That reasoning was about *files*. Bitswap moves blocks, and each block
//! — leaf or interior — is named by the hash of its own bytes, so a 10 MB
//! attachment is forty-odd individually checkable blocks. [`crate::dag`]
//! walks the links; this keeps what the walk produced.
//!
//! So the cap below is per block rather than per item, and every block is
//! still filed under the hash of the bytes that arrived. What did *not*
//! widen is what a challenge can decide: [`crate::challenge`] still draws
//! only from raw CIDs, because holding a DAG completely and proving by
//! hash that you hold it are different claims.

use openfiat_crypto::Cid;
use openfiat_storage::KvStore;

const COLUMN_FAMILY: &str = "pinned_content";

/// The largest single block this node will hold.
///
/// IPFS's standard chunk size, and therefore the size of a full leaf.
/// Measured against a real provider: 262,144 bytes returns a raw CID,
/// 262,145 returns a dag-pb root whose leaves are this size. An interior
/// node is far smaller — a few dozen bytes per link — so this bounds
/// every kind of block a well-formed DAG contains.
pub const MAX_BLOCK_BYTES: usize = 262_144;

pub struct HeldContent<S> {
    store: S,
}

impl<S: KvStore> HeldContent<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }

    /// Stores one block under `cid`, if it genuinely is that block.
    ///
    /// Returns whether it was kept. Refuses silently rather than erroring
    /// on the cases that are ordinary rather than exceptional: a block
    /// larger than any well-formed one, and bytes that are not what the
    /// CID names.
    ///
    /// The codec is not consulted. At block level there is nothing to
    /// consult it about — a dag-pb node's digest covers the node's own
    /// encoding exactly as a raw block's covers its bytes, and it is the
    /// node's encoding that arrives. The distinction that *does* survive
    /// lives in [`crate::challenge`], where the question is about a file.
    pub fn keep(&self, cid: &Cid, block: &[u8]) -> bool {
        if block.len() > MAX_BLOCK_BYTES {
            return false;
        }
        // The last line of defence before this node starts serving these
        // bytes to challengers, and to browsers, as the named content.
        if !cid.matches(block) {
            return false;
        }
        self.store
            .put(COLUMN_FAMILY, cid.as_str().as_bytes(), block)
            .is_ok()
    }

    /// The block this node holds for `cid`, if any.
    pub fn get(&self, cid: &Cid) -> Option<Vec<u8>> {
        self.store
            .get(COLUMN_FAMILY, cid.as_str().as_bytes())
            .ok()
            .flatten()
    }

    /// Whether this node holds the single block `cid` names.
    ///
    /// For a chunked file's root that is not the same as holding the
    /// file — see [`HeldContent::missing_blocks`], which is what a caller
    /// deciding whether it still has fetching to do should ask.
    pub fn holds(&self, cid: &Cid) -> bool {
        self.get(cid).is_some()
    }

    /// What is still needed before this node holds all of `root`.
    ///
    /// Empty means the whole DAG is here. Otherwise it is the blocks to
    /// ask for next: the root itself if it is absent, or — once the root
    /// has arrived and its links can be read — whichever children are
    /// missing, and so on down as each level lands. A node fetching from
    /// its peers therefore walks a level per round rather than needing a
    /// protocol that can ask for a subtree.
    ///
    /// Bounded by [`crate::dag::MAX_DAG_BLOCKS`], so a root that arrived
    /// naming an unreasonable DAG costs a bounded sweep rather than an
    /// unbounded one every tick.
    pub fn missing_blocks(&self, root: &Cid) -> Vec<Cid> {
        let mut missing = Vec::new();
        self.walk(root, &mut |cid, block| {
            if block.is_none() {
                missing.push(cid.clone());
            }
        });
        missing
    }

    /// Every block of `root` this node currently holds, root included.
    fn reachable(&self, root: &Cid) -> Vec<Cid> {
        let mut present = Vec::new();
        self.walk(root, &mut |cid, block| {
            if block.is_some() {
                present.push(cid.clone());
            }
        });
        present
    }

    /// Visits `root` and, for every dag-pb block already here, its links.
    ///
    /// The visitor sees `None` for a block this node does not hold, whose
    /// links are consequently unknown and unvisitable — the two callers
    /// above want opposite halves of exactly that distinction.
    fn walk(&self, root: &Cid, visit: &mut dyn FnMut(&Cid, Option<&[u8]>)) {
        let mut pending = std::collections::VecDeque::from([root.clone()]);
        let mut seen = std::collections::HashSet::from([root.as_str().to_string()]);
        let mut visited = 0usize;

        while let Some(cid) = pending.pop_front() {
            if visited == crate::dag::MAX_DAG_BLOCKS {
                return;
            }
            visited += 1;

            let block = self.get(&cid);
            visit(&cid, block.as_deref());
            let Some(block) = block else { continue };
            if !crate::dag::is_chunked(&cid) {
                continue;
            }
            // A stored block was checked against its CID before it was
            // stored, so unreadable links here mean a dag-pb node that
            // genuinely is malformed rather than one that was tampered
            // with in transit. Either way there is nothing to follow.
            for child in crate::dag::links(&block).unwrap_or_default() {
                if seen.insert(child.as_str().to_string()) {
                    pending.push_back(child);
                }
            }
        }
    }

    /// Drops blocks this node is no longer committed to keeping.
    ///
    /// `keep_roots` is what the caller's retention window still covers —
    /// the CIDs its attachment records name — and everything not reachable
    /// from one of them goes. Passing the survivors rather than the
    /// casualties is deliberate: the caller derives them from the records
    /// it holds, and a node whose records have not replicated yet would
    /// otherwise evict content it should keep.
    ///
    /// Roots rather than blocks for the same reason. A caller that had to
    /// enumerate a chunked file's leaves in order to save them would be
    /// a caller that drops the leaves the moment it cannot parse the root.
    ///
    /// Returns how many were dropped.
    pub fn evict_outside(&self, keep_roots: &[Cid]) -> usize {
        let keep: std::collections::HashSet<String> = keep_roots
            .iter()
            .flat_map(|root| self.reachable(root))
            .map(|cid| cid.as_str().to_string())
            .collect();
        let mut dropped = 0;
        for (key, _) in self
            .store
            .iter_prefix(COLUMN_FAMILY, &[])
            .unwrap_or_default()
        {
            let Ok(held) = std::str::from_utf8(&key) else {
                continue;
            };
            if !keep.contains(held) && self.store.delete(COLUMN_FAMILY, &key).is_ok() {
                dropped += 1;
            }
        }
        dropped
    }

    /// How many blocks are held — for `getNodeInfo`-style reporting, so an
    /// operator can see their node is actually doing the thing it is
    /// being paid a premium for. Blocks and not files, because a file is
    /// what a record names and a block is what this node stores and
    /// serves.
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
    fn refuses_a_block_larger_than_any_well_formed_one() {
        // 256 KiB + 1: no chunker emits a leaf this big, so a peer
        // offering one is asking this node to store something for reasons
        // of its own.
        let store = held();
        let oversized = vec![0u8; MAX_BLOCK_BYTES + 1];
        assert!(!store.keep(&fixtures::probe_cid(), &oversized));
    }

    #[test]
    fn a_dag_pb_block_is_kept_because_its_digest_covers_its_own_bytes() {
        // The change this store exists to make: a chunked file's root and
        // interior nodes are held like any other block, so the file
        // survives the pinning service that first published it. A
        // challenge still cannot be drawn from this CID — see
        // `crate::challenge` — and holding it does not pretend otherwise.
        let store = held();
        let block = crate::dag::test_support::node(&[&fixtures::probe_cid()]);
        let root = crate::dag::test_support::dag_cid(&block);
        assert!(!root.is_verifiable(), "a dag-pb root, not a raw one");

        assert!(store.keep(&root, &block));
        assert_eq!(store.get(&root), Some(block));
    }

    #[test]
    fn a_block_filed_under_a_cid_that_does_not_name_it_is_still_refused() {
        // Widening the codecs must not widen what may be stored under a
        // CID other people's records point at.
        let store = held();
        let block = crate::dag::test_support::node(&[&fixtures::probe_cid()]);
        let root = crate::dag::test_support::dag_cid(&block);
        assert!(!store.keep(&root, b"not that node"));
        assert!(!store.holds(&root));
    }

    #[test]
    fn eviction_drops_what_is_no_longer_covered_and_keeps_what_is() {
        let store = held();
        let kept = fixtures::probe_cid();
        let stale = fixtures::other_cid();
        assert!(store.keep(&kept, PROBE_CONTENT));
        assert!(store.keep(&stale, b"a second attachment"));
        assert_eq!(store.count(), 2);

        assert_eq!(store.evict_outside(std::slice::from_ref(&kept)), 1);
        assert!(store.holds(&kept));
        assert!(!store.holds(&stale));
    }

    #[test]
    fn evicting_against_an_empty_window_drops_everything() {
        // A node reconfigured to keep nothing it still has records for
        // must actually release the disk, not quietly hold on.
        let store = held();
        assert!(store.keep(&fixtures::probe_cid(), PROBE_CONTENT));
        assert_eq!(store.evict_outside(&[]), 1);
        assert_eq!(store.count(), 0);
    }

    #[test]
    fn eviction_is_idempotent() {
        let store = held();
        assert!(store.keep(&fixtures::probe_cid(), PROBE_CONTENT));
        assert_eq!(store.evict_outside(&[]), 1);
        assert_eq!(store.evict_outside(&[]), 0, "a second sweep drops nothing");
    }

    #[test]
    fn a_chunked_file_is_missing_its_root_first_and_then_its_leaves() {
        // A node fetching from peers learns the shape of a DAG one level
        // at a time, because until the root arrives there is nothing that
        // says what the leaves are.
        let store = held();
        let leaf = fixtures::probe_cid();
        let block = crate::dag::test_support::node(&[&leaf]);
        let root = crate::dag::test_support::dag_cid(&block);

        assert_eq!(store.missing_blocks(&root), vec![root.clone()]);
        assert!(store.keep(&root, &block));
        assert_eq!(store.missing_blocks(&root), vec![leaf.clone()]);
        assert!(store.keep(&leaf, PROBE_CONTENT));
        assert!(store.missing_blocks(&root).is_empty());
    }

    #[test]
    fn a_root_present_without_its_leaves_is_not_a_file_this_node_holds() {
        // `holds` answers about one block and would say yes here, which is
        // why the fetch loop asks `missing_blocks` instead.
        let store = held();
        let block = crate::dag::test_support::node(&[&fixtures::probe_cid()]);
        let root = crate::dag::test_support::dag_cid(&block);
        assert!(store.keep(&root, &block));

        assert!(store.holds(&root));
        assert_eq!(store.missing_blocks(&root), vec![fixtures::probe_cid()]);
    }

    #[test]
    fn eviction_keeps_the_leaves_of_a_root_it_was_told_to_keep() {
        // The caller passes the CIDs its records name, which for a chunked
        // file is only the root. A sweep that took that literally would
        // delete every leaf on the next tick and refetch them on the one
        // after, forever.
        let store = held();
        let leaf = fixtures::probe_cid();
        let block = crate::dag::test_support::node(&[&leaf]);
        let root = crate::dag::test_support::dag_cid(&block);
        assert!(store.keep(&root, &block));
        assert!(store.keep(&leaf, PROBE_CONTENT));
        assert!(store.keep(&fixtures::other_cid(), b"a second attachment"));

        assert_eq!(store.evict_outside(std::slice::from_ref(&root)), 1);
        assert!(store.holds(&root) && store.holds(&leaf));
        assert!(!store.holds(&fixtures::other_cid()));
    }

    #[test]
    fn eviction_drops_the_leaves_of_a_root_that_fell_out_of_the_window() {
        let store = held();
        let leaf = fixtures::probe_cid();
        let block = crate::dag::test_support::node(&[&leaf]);
        let root = crate::dag::test_support::dag_cid(&block);
        assert!(store.keep(&root, &block));
        assert!(store.keep(&leaf, PROBE_CONTENT));

        assert_eq!(store.evict_outside(&[]), 2, "the root and its leaf");
        assert_eq!(store.count(), 0);
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
