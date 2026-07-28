//! Drives one node's snapshot index: applies incoming gossip events
//! automatically and provides the operation that originates new ones.
//! Provider registration reuses `openfiat-registry` directly — this
//! crate has no registration event of its own.

use crate::codec;
use crate::error::SnapshotError;
use crate::events::SignedSnapshotAnnounce;
use crate::protocol;
use crate::record::{CompressionMethod, SnapshotId, SnapshotMetadata};
use crate::store::SnapshotIndex;
use openfiat_gossip::GossipService;
use openfiat_registry::Registry;
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{EventType, Priority, Timestamp};
use std::rc::Rc;

pub struct SnapshotService<S> {
    pub gossip: GossipService<S>,
    registry: Rc<SnapshotIndex<S>>,
}

impl<S: KvStore + 'static> SnapshotService<S> {
    /// `services` is the shared handle from `RegistryService::registry`
    /// on the same node — see `SnapshotIndex`.
    pub fn new(mut gossip: GossipService<S>, store: S, services: Rc<Registry<S>>) -> Self {
        let registry = Rc::new(SnapshotIndex::new(store, services));
        let handler_registry = Rc::clone(&registry);
        gossip.add_event_handler(move |event| handler_registry.apply_event(event));
        Self { gossip, registry }
    }

    pub fn get(&self, id: &SnapshotId) -> Option<SnapshotMetadata> {
        self.registry.get(id)
    }

    pub fn all(&self) -> Vec<SnapshotMetadata> {
        self.registry.all()
    }

    pub fn latest(&self) -> Option<SnapshotMetadata> {
        self.registry.latest()
    }

    pub fn checkpoint_height(&self) -> Option<u64> {
        self.registry.checkpoint_height()
    }

    pub fn import(&self, metadata: &SnapshotMetadata, compressed_bytes: &[u8]) -> Result<Vec<u8>, SnapshotError> {
        self.registry.import(metadata, compressed_bytes)
    }

    /// §11: generate and announce a snapshot of `state_bytes` (already
    /// assembled by the caller from whatever registries it wants to
    /// include — see the `record` module doc) under this node's own
    /// identity. `height` is `[PROPOSED — NEEDS SIGN-OFF]`'d to this
    /// node's local gossip event count at generation time; see
    /// `record::SnapshotMetadata::height`.
    pub fn announce(&mut self, id: impl Into<String>, state_bytes: &[u8], height: u64) -> Result<(SnapshotId, Vec<u8>), SnapshotError> {
        let compression = CompressionMethod::None;
        let compressed = codec::compress(state_bytes, compression)?;
        let metadata = SnapshotMetadata {
            id: SnapshotId::new(id),
            snapshot_version: 1,
            protocol_version: protocol::SUPPORTED_PROTOCOL_VERSION,
            height,
            created_at: Timestamp::now(),
            state_root: codec::state_root(state_bytes),
            size_bytes: compressed.len() as u64,
            compression,
            producer: self.gossip.node.local_peer_id(),
            producer_public_key: self.gossip.public_key(),
        };
        let bytes = wire::to_bytes(&metadata).map_err(|_| SnapshotError::MalformedRecord)?;
        let signed = SignedSnapshotAnnounce { signature: self.gossip.sign(&bytes), metadata };
        self.originate(&signed)?;
        Ok((signed.metadata.id, compressed))
    }

    fn originate(&mut self, payload: &impl serde::Serialize) -> Result<(), SnapshotError> {
        let bytes = wire::to_bytes(payload).map_err(|_| SnapshotError::MalformedRecord)?;
        let event_type = EventType::new(protocol::EVENT_ANNOUNCED).expect("snapshot event name is a valid PascalCase identifier");
        self.gossip
            .originate(event_type, protocol::OFS_SPEC, Priority::Snapshot, 8, bytes)
            .map(|_| ())
            .map_err(|_| SnapshotError::Unauthorized)
    }

    pub async fn drive_once(&mut self) {
        self.gossip.drive_once().await;
    }
}
