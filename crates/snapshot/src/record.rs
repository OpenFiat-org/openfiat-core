//! The snapshot metadata shape (OFS-1300 §8-10).
//!
//! §2: this specification explicitly does not define "marketplace
//! logic" — what's actually inside a snapshot's state bytes is every
//! other crate's concern (advertisements, reputation, governance, ...),
//! not this one's. `SnapshotMetadata` describes a snapshot; the state it
//! describes stays opaque to this crate, which handles it only as column
//! families, keys, and values (see [`crate::state`]) and never as
//! meaning.

use crate::location::SnapshotLocation;
use openfiat_types::{PeerId, PublicKey, Timestamp};

/// §15: "the compression method MUST be recorded within snapshot
/// metadata." Only `None` is actually implemented by
/// `crate::compress`/`crate::decompress` today — `Zstd`/`Gzip` are
/// modeled so metadata stays protocol-complete, but this crate reports
/// `SnapshotError::UnsupportedCompression` rather than silently
/// mishandling a snapshot tagged with either.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CompressionMethod {
    None,
    Zstd,
    Gzip,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct SnapshotId(String);

impl SnapshotId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// §8's required metadata fields.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SnapshotMetadata {
    pub id: SnapshotId,
    pub snapshot_version: u32,
    /// §8's "OFS Version" — the protocol/schema version the snapshotted
    /// state was produced under; see `protocol::SUPPORTED_PROTOCOL_VERSION`.
    pub protocol_version: u32,
    /// The Solana slot this snapshot's state is current as of.
    ///
    /// # Why a slot, and not a height
    ///
    /// This was the producing node's own local gossip event count, which
    /// could not do the job asked of it. That number is **per-producer**:
    /// two nodes holding identical state report different values if they
    /// have seen different numbers of events, and a node that joined late
    /// reports a lower one than a node running since genesis. So comparing
    /// two producers' numbers compared nothing, and the anti-rollback
    /// check in `SnapshotIndex::import` was comparing another node's event
    /// count against this node's own — different quantities entirely.
    ///
    /// A Solana slot is the one clock every participant already shares and
    /// agrees on. Ordering two snapshots by it is meaningful whoever
    /// produced them, and unlike a self-reported counter a claimed slot is
    /// **checkable**: a node can compare it against its own view of the
    /// chain and refuse one from an implausible future.
    ///
    /// # What it does and does not assert
    ///
    /// It says *when* the state was captured, not *what* it contains. Two
    /// nodes snapshotting at the same slot may hold slightly different
    /// gossip state, because propagation is not instant. Slot is a recency
    /// anchor, not a proof of containment — the same thing Solana's own
    /// snapshots mean by it. Establishing containment would need consensus
    /// over off-chain state, which this protocol deliberately does not
    /// have.
    ///
    /// A node that has never observed a slot cannot produce a snapshot. It
    /// does not know what time it is, so it cannot honestly say when its
    /// state is from — and a `GossipOnly` node learns slots over the chain
    /// bridge, so this is not a requirement to run an RPC connection.
    pub slot: u64,
    pub created_at: Timestamp,
    /// §10: a cryptographic digest of the *uncompressed* state bytes,
    /// verified after decompression on import.
    pub state_root: [u8; 32],
    /// §8: the *compressed* size, checked against the actual download —
    /// and, before that, used as a hard cap on how many bytes a mirror is
    /// allowed to send at all (see [`crate::fetch::download`]).
    pub size_bytes: u64,
    pub compression: CompressionMethod,
    /// Where these bytes can be downloaded. Not in OFS-1300 §8's field
    /// list, and this workspace's addition: §8 requires a producer to
    /// state a snapshot's size and digest but never where it is, so a
    /// joining node could verify a hash it had no way to obtain and every
    /// announced snapshot was undownloadable by construction.
    ///
    /// Ordered by the producer's preference — globally reachable hosts
    /// before private ones (see [`crate::reachable`]) — and a consumer
    /// tries them in that order. Any one of them that verifies is as good
    /// as any other.
    ///
    /// Never empty in a snapshot this implementation produced: a node with
    /// nowhere to be fetched from declines to write one at all, rather
    /// than announcing a snapshot with no way to obtain it.
    pub locations: Vec<SnapshotLocation>,
    pub producer: PeerId,
    pub producer_public_key: PublicKey,
}
