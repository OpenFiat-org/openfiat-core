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
    pub fn new(
        gossip: GossipService<S>,
        store: S,
        services: Rc<Registry<S>>,
        verify_entry: crate::state::EntryVerifier,
    ) -> Self {
        Self::with_anchors(
            gossip,
            store,
            services,
            crate::trust::TrustAnchors::pinned(),
            verify_entry,
        )
    }

    /// The pinned trust anchors plus whatever the operator added.
    ///
    /// Only the anchors decide whose snapshot a node with *no checkpoint*
    /// will adopt; a node that already has history uses registration, as
    /// before. See `crate::trust`.
    pub fn with_anchors(
        mut gossip: GossipService<S>,
        store: S,
        services: Rc<Registry<S>>,
        anchors: crate::trust::TrustAnchors,
        verify_entry: crate::state::EntryVerifier,
    ) -> Self {
        let registry = Rc::new(SnapshotIndex::with_anchors(
            store,
            services,
            anchors,
            verify_entry,
        ));
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

    pub fn checkpoint_slot(&self) -> Option<u64> {
        self.registry.checkpoint_slot()
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
        (self.gossip.node.local_peer_id(), self.gossip.public_key())
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
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::SNAPSHOT_ANNOUNCE,
            &metadata,
        )
        .map_err(|_| SnapshotError::MalformedRecord)?;
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
    /// Where peers are told to fetch it comes from
    /// [`SnapshotConfig::locations`], applied to the addresses this node's
    /// own gossip transport has learned it is reachable at — so a node
    /// announces a downloadable snapshot without being configured to.
    ///
    /// `slot` is the Solana slot this node's state is current as of, and
    /// is supplied by the caller because this service has no chain access
    /// of its own. It must be a slot the node has genuinely observed — see
    /// `record::SnapshotMetadata::slot` for why a self-invented number
    /// cannot do this job.
    pub fn produce_and_announce(
        &mut self,
        store: &S,
        column_families: &[&str],
        config: &SnapshotConfig,
        slot: u64,
    ) -> Result<SnapshotMetadata, SnapshotError> {
        let (producer, producer_public_key) = self.identity();
        let base_urls = config.locations(&self.gossip.reachable_addresses());
        let produced = crate::producer::produce(
            store,
            column_families,
            config,
            &base_urls,
            slot,
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
