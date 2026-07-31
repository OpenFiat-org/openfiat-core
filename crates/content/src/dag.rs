//! A chunked file, as the blocks it is actually made of.
//!
//! # Why a node can hold more than 256 KiB after all
//!
//! A provider hands back a raw CID for anything up to IPFS's chunk size
//! and a dag-pb root above it. The root's digest covers the DAG node's
//! encoding rather than the file, so nothing that hashes the bytes of a
//! large file can decide whether they are the right ones — which is why
//! [`crate::challenge`] draws only from raw CIDs, and why it still does.
//!
//! But *holding* is not *challenging*. Bitswap moves blocks, not files,
//! and every block — leaf or interior — is named by the hash of its own
//! bytes. So a large file is a set of individually verifiable blocks with
//! one unverifiable label on the outside, and a node can hold every one of
//! them without ever being asked to trust anybody:
//!
//! - the root block is checked against the root CID (its digest *does*
//!   cover the node's own encoding, which is exactly what arrives);
//! - each link is followed to a child block that is checked against the
//!   child's own CID, never against the CID its parent claimed.
//!
//! What the root CID cannot tell us is what the reassembled file looks
//! like — a provider could hand out a root naming blocks that spell out
//! something other than what was uploaded. That is a claim about the
//! uploader, not about this node, and it is unchanged by anything here:
//! the same root would be in the signed attachment record either way.
//! Widening what a node keeps is not widening what a challenge can decide.
//!
//! # The size of a DAG is not knowable from its root
//!
//! A CID is 36 bytes and says nothing about how much content hangs off
//! it. A walk that discovered the size as it went would be a walk whose
//! cost is chosen by whoever published the CID. So the caps below are
//! fixed before the first fetch and the walk abandons the whole DAG the
//! moment it crosses one — a partial DAG is not a file, so there is
//! nothing to salvage by continuing.

use crate::pinning::PinError;
use openfiat_crypto::Cid;

/// The largest a DAG may add up to, over every block.
///
/// [`crate::MAX_ATTACHMENT_BYTES`], because that is what a record is
/// allowed to declare and therefore the largest file this protocol asks
/// anyone to keep. A root naming more than this is either not one of our
/// attachments or is lying about being one.
pub const MAX_DAG_BYTES: usize = crate::MAX_ATTACHMENT_BYTES as usize;

/// The most blocks one DAG may consist of.
///
/// [`MAX_DAG_BYTES`] alone does not bound the work: a million one-byte
/// blocks is well under it and is a million round trips. At IPFS's usual
/// 256 KiB chunking a 10 MB attachment is about forty leaves and one
/// root, so this leaves room for a provider that chunks an order of
/// magnitude smaller without leaving room for one that chunks absurdly.
pub const MAX_DAG_BLOCKS: usize = 512;

/// The most links one dag-pb node may declare.
///
/// Kubo's default fan-out is 174. A node past this bound would have to be
/// answered by fetching that many children, and [`MAX_DAG_BLOCKS`] would
/// stop the walk anyway; refusing at the parse means the refusal names the
/// thing that was wrong.
pub const MAX_LINKS_PER_NODE: usize = 1024;

/// Whether `cid` names a DAG node rather than a file's bytes.
///
/// [`Cid::parse`] accepts exactly two codecs, raw and dag-pb, so "not
/// raw" is "dag-pb". `is_verifiable` is the accessor for the first, and
/// it is deliberately the only one: a second predicate meaning the same
/// thing under a different name is a second place for the codec rule to
/// drift.
pub fn is_chunked(cid: &Cid) -> bool {
    !cid.is_verifiable()
}

/// The CIDs a dag-pb node links to, in the order it lists them.
///
/// `PBNode { bytes Data = 1; repeated PBLink Links = 2 }` and
/// `PBLink { bytes Hash = 1; string Name = 2; uint64 Tsize = 3 }`. Only
/// `Hash` survives: a name is unixfs's business and a `Tsize` is the
/// publisher's claim about a subtree this walk is going to measure for
/// itself, so representing either would be keeping a field nothing acts
/// on. `Data` is likewise skipped — for an interior node it is unixfs
/// framing, and for a leaf it is the content, which this module never
/// needs to interpret.
///
/// Returns `None` for anything that is not a well-formed dag-pb node,
/// *including* a node with a link this protocol cannot address. That is
/// the opposite of [`crate::bitswap::Message::decode`], which drops
/// unusable entries and keeps going, and the difference is what the two
/// are for: one unreadable wantlist entry is a peer with broader tastes,
/// while one unfollowable link is a hole in the middle of a file. A DAG
/// that cannot be walked completely must not look like one that was.
pub fn links(block: &[u8]) -> Option<Vec<Cid>> {
    let mut reader = Reader::new(block);
    let mut links = Vec::new();

    while let Some((field, wire)) = reader.tag()? {
        match (field, wire) {
            (2, 2) => {
                if links.len() == MAX_LINKS_PER_NODE {
                    return None;
                }
                links.push(link_hash(reader.length_delimited()?)?);
            }
            _ => reader.skip(wire)?,
        }
    }

    Some(links)
}

/// The `Hash` field of one `PBLink`, as a CID.
fn link_hash(input: &[u8]) -> Option<Cid> {
    let mut reader = Reader::new(input);
    let mut hash = None;
    while let Some((field, wire)) = reader.tag()? {
        match (field, wire) {
            (1, 2) => hash = Some(Cid::from_binary(reader.length_delimited()?).ok()?),
            _ => reader.skip(wire)?,
        }
    }
    hash
}

/// Somewhere one block can be fetched from by CID.
///
/// The walk below is the same whether the blocks come from a gateway, a
/// peer, or a test's map, and none of those belong in it.
#[async_trait::async_trait(?Send)]
pub trait BlockFetcher {
    /// Retrieves the block `cid` names, having checked that the bytes
    /// hash to it. An implementation that skipped that check would make
    /// every guarantee in this module's documentation false.
    async fn block(&self, cid: &Cid) -> Result<Vec<u8>, PinError>;
}

/// Every block of the DAG rooted at `root`, root first.
///
/// A raw CID is a DAG of one block and comes back as such, so a caller
/// does not need to know which kind it holds.
///
/// Fails as a whole rather than in part. [`PinError::TooLarge`] means the
/// DAG crossed [`MAX_DAG_BYTES`] or [`MAX_DAG_BLOCKS`]; anything else is
/// a block that could not be fetched or did not hash to the CID that
/// named it. In every case nothing is returned, because half a file is
/// not a smaller file — it is bytes that would be served to a peer as
/// though they were the content, and fail.
pub async fn fetch(
    fetcher: &dyn BlockFetcher,
    root: &Cid,
) -> Result<Vec<(Cid, Vec<u8>)>, PinError> {
    let mut blocks: Vec<(Cid, Vec<u8>)> = Vec::new();
    let mut pending = std::collections::VecDeque::from([root.clone()]);
    let mut seen: std::collections::HashSet<String> =
        std::collections::HashSet::from([root.as_str().to_string()]);
    let mut total = 0usize;

    // Breadth-first over an explicit queue rather than by recursion: the
    // depth of a stranger's DAG is a number they choose, and a stack is
    // not something a bound can be enforced against after the fact.
    while let Some(cid) = pending.pop_front() {
        if blocks.len() == MAX_DAG_BLOCKS {
            return Err(PinError::TooLarge);
        }

        let block = fetcher.block(&cid).await?;

        // Every implementation of [`BlockFetcher`] is required to have
        // checked this already, and it is checked again here anyway. The
        // fetcher is the one part of this that a deployment swaps out —
        // a gateway today, a peer or an operator's own daemon tomorrow —
        // and the property being defended is that a child block is only
        // ever kept under the hash of the bytes that arrived, never under
        // the identifier its parent claimed for it. A guarantee that holds
        // only as long as every future implementer read the trait
        // documentation is not the kind of guarantee this deserves.
        if !cid.matches(&block) {
            return Err(PinError::ContentMismatch);
        }

        total = total.saturating_add(block.len());
        if total > MAX_DAG_BYTES {
            return Err(PinError::TooLarge);
        }

        if is_chunked(&cid) {
            for child in links(&block).ok_or(PinError::ContentMismatch)? {
                // A DAG that names the same block twice is legal and
                // cheap to hold once. Deduplicating also means a repeated
                // link cannot be used to multiply the walk's cost.
                if seen.insert(child.as_str().to_string()) {
                    pending.push_back(child);
                }
            }
        }
        blocks.push((cid, block));
    }

    Ok(blocks)
}

/// The file a DAG spells out, in link order.
///
/// [`fetch`]'s sibling, and the difference between them is the whole
/// reason both exist. `fetch` collects the blocks a node must *hold*, so
/// it walks breadth-first and deduplicates. This produces the bytes a
/// browser must *see*, so it walks depth-first in the order each node
/// lists its links — the order the file was chunked in — and a block
/// linked twice contributes its bytes twice, because that is a file with
/// a repeated chunk rather than a walk going round in circles.
///
/// Synchronous, over a lookup rather than a [`BlockFetcher`]: the caller
/// is the node itself reading its own store, where every block is already
/// present or is not coming. Each one is checked against the CID that
/// named it anyway — the store is trusted to be a store, not to be
/// uncorrupted.
///
/// Only a raw block contributes bytes. A dag-pb node's digest covers its
/// own protobuf encoding, which is framing rather than content, so an
/// interior node is followed and never concatenated. A dag-pb node with
/// no links is therefore a leaf this cannot read — a unixfs file whose
/// bytes live in its `Data` field — and it is refused rather than
/// contributing nothing, because a file silently short by one chunk is
/// worse than a file that failed to load.
pub fn assemble<F>(root: &Cid, block: F) -> Result<Vec<u8>, PinError>
where
    F: Fn(&Cid) -> Option<Vec<u8>>,
{
    let mut file = Vec::new();
    // Reverse-ordered stack rather than recursion, for the same reason
    // `fetch` uses a queue: the depth of a DAG is chosen by whoever
    // published it.
    let mut pending = vec![root.clone()];
    let mut visited = 0usize;

    while let Some(cid) = pending.pop() {
        visited += 1;
        if visited > MAX_DAG_BLOCKS {
            return Err(PinError::TooLarge);
        }

        let bytes = block(&cid).ok_or_else(|| {
            PinError::Unavailable(format!("this node does not hold {}", cid.as_str()))
        })?;
        if !cid.matches(&bytes) {
            return Err(PinError::ContentMismatch);
        }

        if !is_chunked(&cid) {
            if file.len().saturating_add(bytes.len()) > MAX_DAG_BYTES {
                return Err(PinError::TooLarge);
            }
            file.extend_from_slice(&bytes);
            continue;
        }

        let links = links(&bytes).ok_or(PinError::ContentMismatch)?;
        if links.is_empty() {
            return Err(PinError::Unavailable(
                "a dag-pb leaf carries its bytes in unixfs framing this protocol does not read"
                    .to_string(),
            ));
        }
        pending.extend(links.into_iter().rev());
    }

    Ok(file)
}

/// A protobuf cursor, returning `None` rather than panicking on anything
/// malformed. The same shape as [`crate::bitswap::message`]'s, and for the
/// same reason: every byte it reads came from a stranger.
struct Reader<'a> {
    input: &'a [u8],
}

impl<'a> Reader<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self { input }
    }

    /// The next `(field number, wire type)`. `Some(None)` is a clean end
    /// of input; `None` is malformed.
    #[allow(clippy::type_complexity)]
    fn tag(&mut self) -> Option<Option<(u64, u8)>> {
        if self.input.is_empty() {
            return Some(None);
        }
        let key = self.varint()?;
        Some(Some((key >> 3, (key & 0x7) as u8)))
    }

    fn varint(&mut self) -> Option<u64> {
        let mut value: u64 = 0;
        for index in 0..10 {
            let byte = *self.input.first()?;
            self.input = &self.input[1..];
            value |= u64::from(byte & 0x7f) << (index * 7);
            if byte & 0x80 == 0 {
                return Some(value);
            }
        }
        None
    }

    fn length_delimited(&mut self) -> Option<&'a [u8]> {
        let length = usize::try_from(self.varint()?).ok()?;
        if length > self.input.len() {
            return None;
        }
        let (taken, rest) = self.input.split_at(length);
        self.input = rest;
        Some(taken)
    }

    fn skip(&mut self, wire: u8) -> Option<()> {
        match wire {
            0 => self.varint().map(|_| ()),
            1 => self.take(8),
            2 => self.length_delimited().map(|_| ()),
            5 => self.take(4),
            // Wire types 3 and 4 are protobuf's removed group encoding and
            // 6/7 never existed: there is no length to skip past.
            _ => None,
        }
    }

    fn take(&mut self, count: usize) -> Option<()> {
        if self.input.len() < count {
            return None;
        }
        self.input = &self.input[count..];
        Some(())
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// Assembles a dag-pb node linking to `children`.
    ///
    /// Field tags are written as the literal bytes the spec calls for —
    /// `0x12` is field 2, wire type 2 — rather than through a helper, so a
    /// fixture cannot agree with the parser by sharing its mistakes. The
    /// one thing that anchors both to reality is
    /// `the_real_empty_directory_block_parses_as_a_node_with_no_links`
    /// below, which uses a block IPFS has served since 2015.
    pub fn node(children: &[&Cid]) -> Vec<u8> {
        let mut out = Vec::new();
        for child in children {
            let hash = child.to_binary();
            let mut link = vec![0x0a, hash.len() as u8]; // field 1, bytes
            link.extend_from_slice(&hash);
            link.extend_from_slice(&[0x12, 0x00]); // field 2, an empty name
            out.push(0x12); // field 2 of PBNode, wire type 2
            out.push(link.len() as u8);
            out.extend_from_slice(&link);
        }
        // Field 1, `Data`: unixfs `Type = File`, which a real root carries
        // and this parser must skip past rather than trip over.
        out.extend_from_slice(&[0x0a, 0x02, 0x08, 0x02]);
        out
    }

    /// The dag-pb CID naming `block` — the hash of the bytes themselves,
    /// which is the only way a block is ever addressed here.
    pub fn dag_cid(block: &[u8]) -> Cid {
        let mut binary = vec![0x01, 0x70, 0x12, 0x20];
        binary.extend_from_slice(&openfiat_crypto::hash::sha256(block));
        Cid::from_binary(&binary).expect("a dag-pb sha2-256 CID is one this protocol accepts")
    }

    /// The raw CID naming `block` — a leaf, whose digest covers the file
    /// bytes themselves.
    pub fn raw_cid(block: &[u8]) -> Cid {
        let mut binary = vec![0x01, 0x55, 0x12, 0x20];
        binary.extend_from_slice(&openfiat_crypto::hash::sha256(block));
        Cid::from_binary(&binary).expect("a raw sha2-256 CID is one this protocol accepts")
    }

    /// A fetcher over a fixed set of blocks.
    #[derive(Default)]
    pub struct MemoryBlocks {
        blocks: HashMap<String, Vec<u8>>,
        /// Every CID asked for, in order, so a test can assert on what a
        /// walk did and did not go looking for.
        pub asked: RefCell<Vec<String>>,
    }

    impl MemoryBlocks {
        pub fn holding(entries: &[(&Cid, &[u8])]) -> Self {
            Self {
                blocks: entries
                    .iter()
                    .map(|(cid, bytes)| (cid.as_str().to_string(), bytes.to_vec()))
                    .collect(),
                asked: RefCell::new(Vec::new()),
            }
        }

        /// Files `bytes` under `cid` without checking that it names them —
        /// only possible here, which is the point: it is how a test builds
        /// the substitution a real fetcher must refuse.
        pub fn plant(&mut self, cid: &Cid, bytes: &[u8]) {
            self.blocks.insert(cid.as_str().to_string(), bytes.to_vec());
        }
    }

    #[async_trait::async_trait(?Send)]
    impl BlockFetcher for MemoryBlocks {
        async fn block(&self, cid: &Cid) -> Result<Vec<u8>, PinError> {
            self.asked.borrow_mut().push(cid.as_str().to_string());
            let bytes = self
                .blocks
                .get(cid.as_str())
                .cloned()
                .ok_or_else(|| PinError::Unavailable("no such block".into()))?;
            // What every real implementation of this trait must do, and
            // what the walk's guarantee rests on.
            if !cid.matches(&bytes) {
                return Err(PinError::ContentMismatch);
            }
            Ok(bytes)
        }
    }

    /// A fetcher that hands back whatever it was given, unchecked.
    ///
    /// A stand-in for the implementation somebody writes later without
    /// reading the trait's documentation — or for one that is compromised.
    /// The walk must not depend on it having behaved.
    pub struct UncheckedBlocks(pub HashMap<String, Vec<u8>>);

    #[async_trait::async_trait(?Send)]
    impl BlockFetcher for UncheckedBlocks {
        async fn block(&self, cid: &Cid) -> Result<Vec<u8>, PinError> {
            self.0
                .get(cid.as_str())
                .cloned()
                .ok_or_else(|| PinError::Unavailable("no such block".into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{MemoryBlocks, dag_cid, node, raw_cid};
    use super::*;
    use crate::fixtures;

    const PROBE_CONTENT: &[u8] = b"openfiat ipfs probe 1785426891\n";
    const OTHER_CONTENT: &[u8] = b"a second attachment";

    #[test]
    fn the_real_empty_directory_block_parses_as_a_node_with_no_links() {
        // `0a 02 08 01` is the unixfs empty directory, the single most
        // widely replicated dag-pb block in existence: CIDv0
        // QmUNLLsPACCz1vLxQVkXqqLX5R1X345qqfHbsf67hvA3Nn, and the CIDv1
        // spelling below. Nothing in this crate produced either. It is
        // here because a parser checked only against fixtures this crate
        // assembled would agree with itself even if the wire format were
        // wrong.
        let block = [0x0a, 0x02, 0x08, 0x01];
        assert_eq!(
            dag_cid(&block).as_str(),
            "bafybeiczsscdsbs7ffqz55asqdf3smv6klcw3gofszvwlyarci47bgf354"
        );
        assert_eq!(links(&block), Some(Vec::new()));
    }

    #[test]
    fn a_nodes_links_are_read_in_order() {
        let block = node(&[&fixtures::probe_cid(), &fixtures::other_cid()]);
        assert_eq!(
            links(&block),
            Some(vec![fixtures::probe_cid(), fixtures::other_cid()])
        );
    }

    #[test]
    fn a_link_this_protocol_cannot_address_fails_the_whole_node() {
        // A blake3 link: legitimate IPFS, and a block this node could
        // never verify. Reading the node as if it had one fewer child
        // would produce a "complete" DAG with a hole in it.
        let mut link = vec![0x0a, 36, 0x01, 0x55, 0x1e, 0x20];
        link.extend_from_slice(&[7u8; 32]);
        let mut block = vec![0x12, link.len() as u8];
        block.extend_from_slice(&link);
        assert_eq!(links(&block), None);
    }

    #[test]
    fn a_node_with_more_links_than_the_cap_is_refused() {
        let hash = fixtures::probe_cid().to_binary();
        let mut block = Vec::new();
        for _ in 0..MAX_LINKS_PER_NODE + 1 {
            let mut link = vec![0x0a, hash.len() as u8];
            link.extend_from_slice(&hash);
            block.push(0x12);
            block.push(link.len() as u8);
            block.extend_from_slice(&link);
        }
        assert_eq!(links(&block), None);
    }

    #[test]
    fn malformed_bytes_are_not_read_as_a_node_with_no_links() {
        for hostile in [
            vec![0x12, 0xff, 0x00],       // a link longer than what follows
            vec![0x13, 0x00],             // wire type 3, unskippable
            vec![0xff; 12],               // a varint that never ends
            vec![0x12, 0x02, 0x0a, 0x00], // a link whose Hash is empty
        ] {
            assert_eq!(links(&hostile), None, "{hostile:?}");
        }
    }

    #[tokio::test]
    async fn a_raw_cid_is_a_dag_of_one_block() {
        let cid = fixtures::probe_cid();
        let blocks = MemoryBlocks::holding(&[(&cid, PROBE_CONTENT)]);
        assert_eq!(
            fetch(&blocks, &cid).await,
            Ok(vec![(cid, PROBE_CONTENT.to_vec())])
        );
    }

    #[tokio::test]
    async fn a_chunked_file_comes_back_as_its_root_and_every_leaf() {
        let leaves = [fixtures::probe_cid(), fixtures::other_cid()];
        let root_block = node(&[&leaves[0], &leaves[1]]);
        let root = dag_cid(&root_block);
        let blocks = MemoryBlocks::holding(&[
            (&root, &root_block),
            (&leaves[0], PROBE_CONTENT),
            (&leaves[1], OTHER_CONTENT),
        ]);

        let fetched = fetch(&blocks, &root).await.unwrap();
        assert_eq!(fetched[0], (root, root_block));
        assert_eq!(fetched[1], (leaves[0].clone(), PROBE_CONTENT.to_vec()));
        assert_eq!(fetched[2], (leaves[1].clone(), OTHER_CONTENT.to_vec()));
    }

    #[tokio::test]
    async fn an_interior_node_is_followed_to_the_leaves_under_it() {
        // Two levels, which is what a file large enough to exceed one
        // node's fan-out actually looks like.
        let leaf = fixtures::probe_cid();
        let middle_block = node(&[&leaf]);
        let middle = dag_cid(&middle_block);
        let root_block = node(&[&middle]);
        let root = dag_cid(&root_block);

        let blocks = MemoryBlocks::holding(&[
            (&root, &root_block),
            (&middle, &middle_block),
            (&leaf, PROBE_CONTENT),
        ]);
        let fetched = fetch(&blocks, &root).await.unwrap();
        assert_eq!(fetched.len(), 3);
        assert!(fetched.iter().any(|(cid, _)| *cid == leaf));
    }

    #[tokio::test]
    async fn a_child_whose_bytes_are_not_what_its_cid_names_fails_the_dag() {
        // The substitution this whole module turns on: the root is honest
        // and a leaf's bytes are someone else's. Storing them under the
        // CID the parent named is precisely the attack
        // `bitswap::message::decode_block` closes, so the walk must not
        // reopen it — and half a DAG must not be kept either, since the
        // node would then serve a hole to a browser.
        let leaf = fixtures::probe_cid();
        let root_block = node(&[&leaf]);
        let root = dag_cid(&root_block);
        let mut blocks = MemoryBlocks::holding(&[(&root, &root_block)]);
        blocks.plant(&leaf, b"substituted bytes");

        assert_eq!(fetch(&blocks, &root).await, Err(PinError::ContentMismatch));
    }

    #[tokio::test]
    async fn a_fetcher_that_does_not_check_its_own_blocks_is_still_caught() {
        // The same substitution as above, arriving through a fetcher that
        // performs no verification at all — which is what a swapped-in
        // implementation, or a compromised one, looks like. The walk must
        // refuse on its own account and not because the fetcher did.
        use super::test_support::UncheckedBlocks;
        let leaf = fixtures::probe_cid();
        let root_block = node(&[&leaf]);
        let root = dag_cid(&root_block);
        let blocks = UncheckedBlocks(
            [
                (root.as_str().to_string(), root_block),
                (leaf.as_str().to_string(), b"substituted bytes".to_vec()),
            ]
            .into_iter()
            .collect(),
        );

        assert_eq!(fetch(&blocks, &root).await, Err(PinError::ContentMismatch));
    }

    #[tokio::test]
    async fn a_dag_missing_one_leaf_yields_nothing_rather_than_the_rest() {
        let leaves = [fixtures::probe_cid(), fixtures::other_cid()];
        let root_block = node(&[&leaves[0], &leaves[1]]);
        let root = dag_cid(&root_block);
        let blocks = MemoryBlocks::holding(&[(&root, &root_block), (&leaves[0], PROBE_CONTENT)]);

        assert!(matches!(
            fetch(&blocks, &root).await,
            Err(PinError::Unavailable(_))
        ));
    }

    #[tokio::test]
    async fn a_root_naming_more_bytes_than_an_attachment_may_be_is_abandoned() {
        // The cap is fixed before the walk starts, because the only thing
        // the root tells us about the size is nothing at all.
        // One oversized leaf rather than forty honest ones: the budget is
        // a running total either way, and this keeps the fixture about the
        // budget rather than about chunking.
        let oversized = vec![0u8; MAX_DAG_BYTES + 1];
        let leaf = {
            let mut binary = vec![0x01, 0x55, 0x12, 0x20];
            binary.extend_from_slice(&openfiat_crypto::hash::sha256(&oversized));
            Cid::from_binary(&binary).unwrap()
        };
        let root_block = node(&[&leaf]);
        let root = dag_cid(&root_block);
        // The leaf genuinely is what its CID names, so nothing but the cap
        // can be what refuses it.
        let blocks = MemoryBlocks::holding(&[(&root, &root_block), (&leaf, &oversized)]);

        assert_eq!(fetch(&blocks, &root).await, Err(PinError::TooLarge));
    }

    #[tokio::test]
    async fn a_dag_of_more_blocks_than_the_cap_is_abandoned() {
        // Every leaf is one byte, so the byte budget would never fire: the
        // block count is the only thing standing between this node and a
        // walk whose length a stranger chose.
        let mut entries: Vec<(Cid, Vec<u8>)> = Vec::new();
        for index in 0..MAX_DAG_BLOCKS as u32 + 1 {
            let bytes = index.to_le_bytes().to_vec();
            let mut binary = vec![0x01, 0x55, 0x12, 0x20];
            binary.extend_from_slice(&openfiat_crypto::hash::sha256(&bytes));
            entries.push((Cid::from_binary(&binary).unwrap(), bytes));
        }
        let root_block = node(&entries.iter().map(|(cid, _)| cid).collect::<Vec<_>>());
        let root = dag_cid(&root_block);

        let mut held: Vec<(&Cid, &[u8])> = vec![(&root, &root_block)];
        held.extend(entries.iter().map(|(cid, bytes)| (cid, bytes.as_slice())));
        let blocks = MemoryBlocks::holding(&held);

        assert_eq!(fetch(&blocks, &root).await, Err(PinError::TooLarge));
    }

    /// A lookup over a fixed set of blocks, for [`assemble`].
    fn holding(entries: &[(&Cid, &[u8])]) -> impl Fn(&Cid) -> Option<Vec<u8>> + use<> {
        let blocks: std::collections::HashMap<String, Vec<u8>> = entries
            .iter()
            .map(|(cid, bytes)| (cid.as_str().to_string(), bytes.to_vec()))
            .collect();
        move |cid: &Cid| blocks.get(cid.as_str()).cloned()
    }

    #[test]
    fn a_raw_cid_assembles_to_its_own_bytes() {
        let cid = fixtures::probe_cid();
        assert_eq!(
            assemble(&cid, holding(&[(&cid, PROBE_CONTENT)])),
            Ok(PROBE_CONTENT.to_vec())
        );
    }

    #[test]
    fn leaves_are_concatenated_in_the_order_the_dag_lists_them() {
        // The order is the file. Reading these two the other way round
        // produces a byte string that is not the attachment anybody
        // uploaded, and nothing downstream could tell.
        let leaves = [fixtures::probe_cid(), fixtures::other_cid()];
        let root_block = node(&[&leaves[0], &leaves[1]]);
        let root = dag_cid(&root_block);
        let blocks = holding(&[
            (&root, &root_block),
            (&leaves[0], PROBE_CONTENT),
            (&leaves[1], OTHER_CONTENT),
        ]);

        let mut expected = PROBE_CONTENT.to_vec();
        expected.extend_from_slice(OTHER_CONTENT);
        assert_eq!(assemble(&root, blocks), Ok(expected));
    }

    #[test]
    fn a_leaf_beside_an_interior_node_is_read_depth_first_not_level_by_level() {
        // The shape that separates this walk from `fetch`'s. A root
        // listing an interior node *before* a leaf is what unixfs's
        // trickle layout produces, and a breadth-first walk would emit
        // the shallow leaf first — a file whose chunks are in the wrong
        // order, assembled without a single failed hash check.
        let deep = raw_cid(PROBE_CONTENT);
        let middle_block = node(&[&deep]);
        let middle = dag_cid(&middle_block);
        let shallow = raw_cid(OTHER_CONTENT);
        let root_block = node(&[&middle, &shallow]);
        let root = dag_cid(&root_block);

        let blocks = holding(&[
            (&root, &root_block),
            (&middle, &middle_block),
            (&deep, PROBE_CONTENT),
            (&shallow, OTHER_CONTENT),
        ]);

        let mut expected = PROBE_CONTENT.to_vec();
        expected.extend_from_slice(OTHER_CONTENT);
        assert_eq!(assemble(&root, blocks), Ok(expected));
    }

    #[test]
    fn a_block_linked_twice_contributes_its_bytes_twice() {
        // The opposite of `fetch`'s deduplication, and deliberately: a
        // file with two identical chunks stores one block and is still
        // twice as long as that block.
        let leaf = fixtures::probe_cid();
        let root_block = node(&[&leaf, &leaf]);
        let root = dag_cid(&root_block);

        let mut expected = PROBE_CONTENT.to_vec();
        expected.extend_from_slice(PROBE_CONTENT);
        assert_eq!(
            assemble(
                &root,
                holding(&[(&root, &root_block), (&leaf, PROBE_CONTENT)])
            ),
            Ok(expected)
        );
    }

    #[test]
    fn a_leaf_whose_bytes_are_not_what_its_cid_names_fails_the_whole_file() {
        let leaf = fixtures::probe_cid();
        let root_block = node(&[&leaf]);
        let root = dag_cid(&root_block);
        assert_eq!(
            assemble(
                &root,
                holding(&[(&root, &root_block), (&leaf, b"substituted bytes")])
            ),
            Err(PinError::ContentMismatch)
        );
    }

    #[test]
    fn a_file_missing_one_chunk_fails_rather_than_coming_back_short() {
        let leaves = [fixtures::probe_cid(), fixtures::other_cid()];
        let root_block = node(&[&leaves[0], &leaves[1]]);
        let root = dag_cid(&root_block);
        assert!(matches!(
            assemble(
                &root,
                holding(&[(&root, &root_block), (&leaves[0], PROBE_CONTENT)])
            ),
            Err(PinError::Unavailable(_))
        ));
    }

    #[test]
    fn a_dag_pb_leaf_is_refused_rather_than_contributing_nothing() {
        // `0a 02 08 01` is a well-formed dag-pb node with no links, so
        // every hash check passes and the file it names is one this walk
        // cannot read. Returning an empty body here would be a broken
        // image that looks like a successful fetch.
        let block = [0x0a, 0x02, 0x08, 0x01];
        let cid = dag_cid(&block);
        assert!(matches!(
            assemble(&cid, holding(&[(&cid, &block)])),
            Err(PinError::Unavailable(_))
        ));
    }

    #[tokio::test]
    async fn the_same_block_linked_twice_is_fetched_once() {
        let leaf = fixtures::probe_cid();
        let root_block = node(&[&leaf, &leaf]);
        let root = dag_cid(&root_block);
        let blocks = MemoryBlocks::holding(&[(&root, &root_block), (&leaf, PROBE_CONTENT)]);

        let fetched = fetch(&blocks, &root).await.unwrap();
        assert_eq!(fetched.len(), 2);
        assert_eq!(blocks.asked.borrow().len(), 2);
    }
}
