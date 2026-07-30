//! Drives one node's snapshot index: applies incoming gossip events
//! automatically and provides the operation that originates new ones.
//! Provider registration reuses `openfiat-registry` directly — this
//! crate has no registration event of its own.

use crate::config::SnapshotConfig;
use crate::error::SnapshotError;
use crate::events::SignedSnapshotAnnounce;
use crate::protocol;
use crate::record::{SnapshotId, SnapshotMetadata};
use crate::store::SnapshotIndex;
use openfiat_gossip::GossipService;
use openfiat_registry::Registry;
use openfiat_serialization::json;
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{EventType, Priority};
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

    /// The underlying index, for [`crate::fetch::fetch_and_import`] and
    /// anything else that needs the verifying half without this service's
    /// gossip half.
    pub fn index(&self) -> &SnapshotIndex<S> {
        &self.registry
    }

    pub fn import(&self, id: &SnapshotId, compressed_bytes: &[u8]) -> Result<usize, SnapshotError> {
        self.registry.import(id, compressed_bytes)
    }

    /// The node's own identity, for a caller assembling metadata to
    /// announce — see [`announce_produced`](Self::announce_produced).
    pub fn identity(&self) -> (openfiat_types::PeerId, openfiat_types::PublicKey) {
        (
            self.gossip.node.local_peer_id(),
            self.gossip.public_key(),
        )
    }

    /// §11-12: sign and gossip metadata describing a snapshot this node
    /// has already written (see [`crate::producer::produce`]), and record
    /// it in this node's own index.
    ///
    /// Applying locally before originating is deliberate and matches
    /// every other `sendX` path in this workspace: a producer that is not
    /// registered as a snapshot provider gets a real `Unauthorized` back
    /// here, rather than gossiping an announcement the whole cluster
    /// silently drops.
    pub fn announce_produced(
        &mut self,
        metadata: SnapshotMetadata,
    ) -> Result<SnapshotId, SnapshotError> {
        let bytes = json::to_bytes(&metadata).map_err(|_| SnapshotError::MalformedRecord)?;
        let signed = SignedSnapshotAnnounce {
            signature: self.gossip.sign(&bytes),
            metadata,
        };
        let id = self.registry.apply_announce(signed.clone())?;
        self.originate(&signed)?;
        Ok(id)
    }

    /// §11: snapshot this node's own persisted state, write it under
    /// `config.directory`, and announce it. `column_families` is the set
    /// of domain column families that make up this node's worldview —
    /// the composition root that knows them all supplies it (see
    /// `openfiat_rpc::state::SNAPSHOT_COLUMN_FAMILIES`).
    ///
    /// `height` is `[PROPOSED — NEEDS SIGN-OFF]`'d to this node's local
    /// gossip event count at generation time; see
    /// `record::SnapshotMetadata::height`.
    pub fn produce_and_announce(
        &mut self,
        store: &S,
        column_families: &[&str],
        config: &SnapshotConfig,
    ) -> Result<SnapshotMetadata, SnapshotError> {
        let height = self.gossip.event_count() as u64;
        let (producer, producer_public_key) = self.identity();
        let produced = crate::producer::produce(
            store,
            column_families,
            config,
            height,
            producer,
            producer_public_key,
        )?;
        self.announce_produced(produced.metadata.clone())?;
        Ok(produced.metadata)
    }

    fn originate(&mut self, payload: &impl serde::Serialize) -> Result<(), SnapshotError> {
        let bytes = wire::to_bytes(payload).map_err(|_| SnapshotError::MalformedRecord)?;
        let event_type = EventType::new(protocol::EVENT_ANNOUNCED)
            .expect("snapshot event name is a valid PascalCase identifier");
        self.gossip
            .originate(event_type, protocol::OFS_SPEC, Priority::Snapshot, 8, bytes)
            .map(|_| ())
            .map_err(|_| SnapshotError::Unauthorized)
    }

    pub async fn drive_once(&mut self) {
        self.gossip.drive_once().await;
    }
}
