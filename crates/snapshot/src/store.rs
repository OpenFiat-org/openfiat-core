//! The replicated local snapshot index, sharing a handle to the node's
//! service registry (§5/§24: only a registered snapshot provider may
//! announce one), plus the import pipeline (§16-17) and this node's own
//! local checkpoint bookkeeping (§9, §18).

use crate::codec;
use crate::error::SnapshotError;
use crate::events::SignedSnapshotAnnounce;
use crate::protocol;
use crate::record::{SnapshotId, SnapshotMetadata};
use openfiat_registry::Registry;
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{EventEnvelope, InfrastructureService, ServiceType};
use std::rc::Rc;

const SNAPSHOTS_COLUMN_FAMILY: &str = "snapshot_metadata";
const CHECKPOINT_COLUMN_FAMILY: &str = "snapshot_checkpoint";
const CHECKPOINT_KEY: &[u8] = b"local";

pub struct SnapshotIndex<S> {
    store: S,
    services: Rc<Registry<S>>,
}

impl<S: KvStore> SnapshotIndex<S> {
    pub fn new(store: S, services: Rc<Registry<S>>) -> Self {
        Self { store, services }
    }

    pub fn get(&self, id: &SnapshotId) -> Option<SnapshotMetadata> {
        let bytes = self
            .store
            .get(SNAPSHOTS_COLUMN_FAMILY, id.as_str().as_bytes())
            .ok()
            .flatten()?;
        wire::from_bytes(&bytes).ok()
    }

    fn put(&self, metadata: &SnapshotMetadata) {
        if let Ok(bytes) = wire::to_bytes(metadata) {
            let _ = self.store.put(
                SNAPSHOTS_COLUMN_FAMILY,
                metadata.id.as_str().as_bytes(),
                &bytes,
            );
        }
    }

    pub fn all(&self) -> Vec<SnapshotMetadata> {
        self.store
            .iter_prefix(SNAPSHOTS_COLUMN_FAMILY, &[])
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(_, value)| wire::from_bytes(&value).ok())
            .collect()
    }

    /// §13/§20: the highest-height snapshot this node currently knows
    /// about, from any provider.
    pub fn latest(&self) -> Option<SnapshotMetadata> {
        self.all()
            .into_iter()
            .max_by_key(|metadata| metadata.height)
    }

    /// §5/§12/§24: only a registered snapshot provider may announce one,
    /// and every Snapshot ID is permanent.
    pub fn apply_announce(
        &self,
        signed: SignedSnapshotAnnounce,
    ) -> Result<SnapshotId, SnapshotError> {
        signed.verify()?;
        let metadata = signed.metadata;
        if !self.services.all().into_iter().any(|service| {
            service.provider == metadata.producer
                && matches!(
                    service.service_type,
                    ServiceType::Infrastructure(InfrastructureService::SnapshotProvider)
                )
        }) {
            return Err(SnapshotError::Unauthorized);
        }
        if self.get(&metadata.id).is_some() {
            return Err(SnapshotError::DuplicateSnapshotId);
        }

        self.put(&metadata);
        Ok(metadata.id)
    }

    /// §16-17's import pipeline, from "Verify" onward — download and
    /// decompression-transport are the caller's concern (§14); this
    /// checks protocol compatibility, size, and the State Root, then
    /// activates the snapshot by advancing this node's local checkpoint
    /// (§18: "Snapshot Imported → Request Missing Events → ..."), never
    /// regressing it if a lower/equal-height snapshot is imported later.
    pub fn import(
        &self,
        metadata: &SnapshotMetadata,
        compressed_bytes: &[u8],
    ) -> Result<Vec<u8>, SnapshotError> {
        if metadata.protocol_version != protocol::SUPPORTED_PROTOCOL_VERSION {
            return Err(SnapshotError::UnsupportedProtocolVersion);
        }
        if compressed_bytes.len() as u64 != metadata.size_bytes {
            return Err(SnapshotError::SizeMismatch);
        }
        let state_bytes = codec::decompress(compressed_bytes, metadata.compression)?;
        if codec::state_root(&state_bytes) != metadata.state_root {
            return Err(SnapshotError::StateRootMismatch);
        }

        if metadata.height > self.checkpoint_height().unwrap_or(0) {
            let _ = self.store.put(
                CHECKPOINT_COLUMN_FAMILY,
                CHECKPOINT_KEY,
                &metadata.height.to_le_bytes(),
            );
        }
        Ok(state_bytes)
    }

    /// §9/§18: the height of the most recent snapshot this node has
    /// actually imported — where gossip catch-up replay should resume
    /// from, instead of full history replay.
    pub fn checkpoint_height(&self) -> Option<u64> {
        let bytes = self
            .store
            .get(CHECKPOINT_COLUMN_FAMILY, CHECKPOINT_KEY)
            .ok()
            .flatten()?;
        Some(u64::from_le_bytes(bytes.try_into().ok()?))
    }

    pub fn apply_event(&self, event: &EventEnvelope) {
        if event.ofs_spec != protocol::OFS_SPEC
            || event.event_type.as_str() != protocol::EVENT_ANNOUNCED
        {
            return;
        }
        if let Ok(signed) = wire::from_bytes(&event.payload) {
            let _ = self.apply_announce(signed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::CompressionMethod;
    use openfiat_crypto::Keypair;
    use openfiat_network::identity::peer_id_from_public_key;
    use openfiat_registry::{Registration, SignedRegistration};
    use openfiat_storage::mem::MemoryStore;
    use openfiat_types::{ServiceId, Timestamp};

    fn registered_provider(seed: u8) -> (Keypair, Rc<Registry<MemoryStore>>) {
        let keypair = Keypair::from_seed([seed; 32]);
        let services = Rc::new(Registry::new(MemoryStore::new()));
        let registration = Registration {
            service_id: ServiceId::new(format!("snapshot-svc-{seed}")),
            service_type: ServiceType::Infrastructure(InfrastructureService::SnapshotProvider),
            provider: peer_id_from_public_key(&keypair.public_key()).unwrap(),
            provider_public_key: keypair.public_key(),
            endpoints: vec![],
            supported_ofs: vec![1300],
            region: None,
            capabilities: vec![],
            pricing: None,
            payout_wallet: None,
            timestamp: Timestamp::now(),
        };
        services
            .apply_registration(SignedRegistration::sign(registration, &keypair))
            .unwrap();
        (keypair, services)
    }

    fn announce(provider: &Keypair, id: &str, height: u64, state_bytes: &[u8]) -> SnapshotMetadata {
        SnapshotMetadata {
            id: SnapshotId::new(id),
            snapshot_version: 1,
            protocol_version: protocol::SUPPORTED_PROTOCOL_VERSION,
            height,
            created_at: Timestamp::now(),
            state_root: codec::state_root(state_bytes),
            size_bytes: state_bytes.len() as u64,
            compression: CompressionMethod::None,
            producer: peer_id_from_public_key(&provider.public_key()).unwrap(),
            producer_public_key: provider.public_key(),
        }
    }

    #[test]
    fn an_unregistered_announcer_is_rejected() {
        let services = Rc::new(Registry::new(MemoryStore::new()));
        let index = SnapshotIndex::new(MemoryStore::new(), services);
        let stranger = Keypair::generate();
        let result = index.apply_announce(SignedSnapshotAnnounce::sign(
            announce(&stranger, "snap-1", 100, b"state"),
            &stranger,
        ));
        assert_eq!(result, Err(SnapshotError::Unauthorized));
    }

    #[test]
    fn a_registered_provider_can_announce_and_it_is_queryable() {
        let (provider, services) = registered_provider(1);
        let index = SnapshotIndex::new(MemoryStore::new(), services);
        let id = index
            .apply_announce(SignedSnapshotAnnounce::sign(
                announce(&provider, "snap-1", 100, b"state"),
                &provider,
            ))
            .unwrap();
        assert_eq!(index.get(&id).unwrap().height, 100);
    }

    #[test]
    fn latest_picks_the_highest_height() {
        let (provider, services) = registered_provider(1);
        let index = SnapshotIndex::new(MemoryStore::new(), services);
        index
            .apply_announce(SignedSnapshotAnnounce::sign(
                announce(&provider, "snap-1", 100, b"state-1"),
                &provider,
            ))
            .unwrap();
        index
            .apply_announce(SignedSnapshotAnnounce::sign(
                announce(&provider, "snap-2", 250, b"state-2"),
                &provider,
            ))
            .unwrap();
        assert_eq!(index.latest().unwrap().height, 250);
    }

    #[test]
    fn importing_a_valid_snapshot_returns_the_state_and_advances_the_checkpoint() {
        let (provider, services) = registered_provider(1);
        let index = SnapshotIndex::new(MemoryStore::new(), services);
        let metadata = announce(&provider, "snap-1", 4217, b"the full marketplace state");
        index
            .apply_announce(SignedSnapshotAnnounce::sign(metadata.clone(), &provider))
            .unwrap();

        let state = index
            .import(&metadata, b"the full marketplace state")
            .unwrap();
        assert_eq!(state, b"the full marketplace state");
        assert_eq!(index.checkpoint_height(), Some(4217));
    }

    #[test]
    fn a_tampered_state_blob_fails_state_root_verification() {
        let (provider, services) = registered_provider(1);
        let index = SnapshotIndex::new(MemoryStore::new(), services);
        let metadata = announce(&provider, "snap-1", 100, b"the real state");
        let result = index.import(&metadata, b"the real statE");
        assert_eq!(result, Err(SnapshotError::StateRootMismatch));
        assert_eq!(index.checkpoint_height(), None);
    }

    #[test]
    fn a_truncated_download_fails_the_size_check() {
        let (provider, services) = registered_provider(1);
        let index = SnapshotIndex::new(MemoryStore::new(), services);
        let metadata = announce(&provider, "snap-1", 100, b"the full state");
        let result = index.import(&metadata, b"the full sta");
        assert_eq!(result, Err(SnapshotError::SizeMismatch));
    }

    #[test]
    fn importing_an_older_snapshot_does_not_regress_the_checkpoint() {
        let (provider, services) = registered_provider(1);
        let index = SnapshotIndex::new(MemoryStore::new(), services);
        let newer = announce(&provider, "snap-2", 500, b"newer state");
        index.import(&newer, b"newer state").unwrap();

        let older = announce(&provider, "snap-1", 100, b"older state");
        index.import(&older, b"older state").unwrap();
        assert_eq!(index.checkpoint_height(), Some(500));
    }
}
