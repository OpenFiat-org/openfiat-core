//! The replicated local snapshot index, sharing a handle to the node's
//! service registry (§5/§24: only a registered snapshot provider may
//! announce one), plus the import pipeline (§16-17) and this node's own
//! local checkpoint bookkeeping (§9, §18).

use crate::codec;
use crate::error::SnapshotError;
use crate::events::SignedSnapshotAnnounce;
use crate::protocol;
use crate::record::{SnapshotId, SnapshotMetadata};
use crate::trust::TrustAnchors;
use openfiat_registry::Registry;
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{EventEnvelope, InfrastructureService, ServiceType};
use std::rc::Rc;

const SNAPSHOTS_COLUMN_FAMILY: &str = "snapshot_metadata";
const CHECKPOINT_COLUMN_FAMILY: &str = "snapshot_checkpoint";
const CHECKPOINT_KEY: &[u8] = b"local";

/// The column families an imported snapshot may never write — this
/// node's own snapshot bookkeeping. See `crate::state::restore` for the
/// lockout this prevents.
pub const RESERVED_COLUMN_FAMILIES: &[&str] = &[SNAPSHOTS_COLUMN_FAMILY, CHECKPOINT_COLUMN_FAMILY];

pub struct SnapshotIndex<S> {
    store: S,
    services: Rc<Registry<S>>,
    /// Who this node will take a *first* snapshot from, when it has no
    /// checkpoint of its own to judge one against.
    anchors: TrustAnchors,
}

impl<S: KvStore> SnapshotIndex<S> {
    pub fn new(store: S, services: Rc<Registry<S>>) -> Self {
        Self::with_anchors(store, services, TrustAnchors::pinned())
    }

    /// The pinned anchors plus whatever the operator added — see
    /// `crate::trust`. Separate from `new` so the common case cannot
    /// forget them: `new` is anchored too, and there is no constructor
    /// that produces an unanchored index.
    pub fn with_anchors(store: S, services: Rc<Registry<S>>, anchors: TrustAnchors) -> Self {
        Self {
            store,
            services,
            anchors,
        }
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

    /// §5/§24: whether `producer` is on file *right now* as a snapshot
    /// provider.
    ///
    /// Checked at announce time and again at import time rather than
    /// once, because the two can be far apart: a provider whose
    /// registration has since lapsed or been replaced is no longer
    /// someone this node hands its entire worldview to, however good the
    /// signature on the announcement still is.
    pub fn is_registered_provider(&self, producer: &openfiat_types::PeerId) -> bool {
        self.services.all().into_iter().any(|service| {
            &service.provider == producer
                && matches!(
                    service.service_type,
                    ServiceType::Infrastructure(InfrastructureService::SnapshotProvider)
                )
        })
    }

    /// §5/§12/§24: only a registered snapshot provider may announce one,
    /// and every Snapshot ID is permanent.
    pub fn apply_announce(
        &self,
        signed: SignedSnapshotAnnounce,
    ) -> Result<SnapshotId, SnapshotError> {
        signed.verify()?;
        let metadata = signed.metadata;
        if !self.is_registered_provider(&metadata.producer) {
            return Err(SnapshotError::Unauthorized);
        }
        if self.get(&metadata.id).is_some() {
            return Err(SnapshotError::DuplicateSnapshotId);
        }

        self.put(&metadata);
        Ok(metadata.id)
    }

    /// §16-17's import pipeline: verify `compressed_bytes` against the
    /// announcement this node already holds for `id`, write the state
    /// they decode to, and advance this node's local checkpoint (§18:
    /// "Snapshot Imported → Request Missing Events → ..."). Returns how
    /// many state entries were restored.
    ///
    /// A snapshot is the importing node's entire worldview, so accepting
    /// a bad one is strictly worse than failing to start. Every check
    /// below therefore fails closed, and the order is deliberate:
    ///
    /// 1. **`id` must already be announced here.** The metadata comes
    ///    from this node's own index, never from the caller — so it has
    ///    been signature-verified and registry-authorized by
    ///    `apply_announce`. This is what stops a caller handing over
    ///    self-made metadata whose `state_root` merely matches the bytes
    ///    it also supplied; that pairing verifies perfectly and means
    ///    nothing.
    /// 2. **The producer must still be a registered provider** — see
    ///    `is_registered_provider`.
    /// 3. **The snapshot must be newer than what this node already has.**
    ///    Unlike the earlier read-only version of this method, importing
    ///    now *writes state*, so replaying an old snapshot would silently
    ///    roll the node backwards.
    /// 4. **Size, then digest, then write.** Nothing reaches the store
    ///    until the decompressed bytes hash to the announced `state_root`.
    pub fn import(&self, id: &SnapshotId, compressed_bytes: &[u8]) -> Result<usize, SnapshotError> {
        let metadata = self.get(id).ok_or(SnapshotError::UnknownSnapshot)?;
        if metadata.protocol_version != protocol::SUPPORTED_PROTOCOL_VERSION {
            return Err(SnapshotError::UnsupportedProtocolVersion);
        }
        if !self.is_registered_provider(&metadata.producer) {
            return Err(SnapshotError::Unauthorized);
        }
        // A node with no checkpoint has no history to judge a snapshot
        // against, so every check above passes on a well-formed forgery:
        // they establish that the bytes are what the announcer said, not
        // that the announcer is telling the truth. Nothing in the protocol
        // establishes what the correct state root at a height IS. So the
        // first snapshot — and only the first — must come from a pinned
        // anchor. After this import the node has its own basis, and
        // registration governs. See `crate::trust`.
        if self.checkpoint_height().is_none() && !self.anchors.trusts(&metadata.producer) {
            return Err(SnapshotError::UntrustedFirstSnapshot);
        }
        // Only a node that has already imported something can be rolled
        // backwards. A node with no checkpoint has no state to lose and
        // must be able to import *any* height — including 0, which is
        // what a brand-new cluster's first snapshot legitimately carries.
        if self
            .checkpoint_height()
            .is_some_and(|current| metadata.height <= current)
        {
            return Err(SnapshotError::StaleSnapshot);
        }
        if compressed_bytes.len() as u64 != metadata.size_bytes {
            return Err(SnapshotError::SizeMismatch);
        }
        let state_bytes = codec::decompress(compressed_bytes, metadata.compression)?;
        if codec::state_root(&state_bytes) != metadata.state_root {
            return Err(SnapshotError::StateRootMismatch);
        }

        let restored = crate::state::restore(&self.store, &state_bytes, RESERVED_COLUMN_FAMILIES)?;
        // Last, and only once the state it describes is actually on disk:
        // a checkpoint advanced ahead of the state would tell this node's
        // gossip catch-up to resume from a height whose state it does not
        // have.
        self.store
            .put(
                CHECKPOINT_COLUMN_FAMILY,
                CHECKPOINT_KEY,
                &metadata.height.to_le_bytes(),
            )
            .map_err(|_| SnapshotError::StateUnwritable)?;
        Ok(restored)
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
    use crate::location::SnapshotLocation;
    use crate::record::CompressionMethod;
    use crate::state;
    use openfiat_crypto::Keypair;
    use openfiat_network::identity::peer_id_from_public_key;
    use openfiat_registry::{Registration, SignedRegistration};
    use openfiat_storage::mem::MemoryStore;
    use openfiat_types::{ServiceId, Timestamp};

    /// The tests share one physical store between the index and their own
    /// assertions the way a real node does — `Rc<MemoryStore>`, matching
    /// `NodeState`'s single-store composition.
    type TestStore = Rc<MemoryStore>;

    fn registered_provider(seed: u8) -> (Keypair, Rc<Registry<TestStore>>) {
        let keypair = Keypair::from_seed([seed; 32]);
        let services = Rc::new(Registry::new(Rc::new(MemoryStore::new())));
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

    /// An index that trusts `provider` for a first snapshot.
    ///
    /// Most tests here are about the *import pipeline* — size, digest,
    /// height ordering — not about who is trusted, and a randomly seeded
    /// test provider is naturally not a pinned anchor. Adding it through
    /// the real additive path keeps those tests aimed at what they mean to
    /// assert, and keeps the anchor gate itself covered by the tests that
    /// deliberately do NOT use this.
    fn trusting(
        store: TestStore,
        services: Rc<Registry<TestStore>>,
        provider: &Keypair,
    ) -> SnapshotIndex<TestStore> {
        let key = bs58::encode(provider.public_key().as_bytes()).into_string();
        SnapshotIndex::with_anchors(
            store,
            services,
            crate::trust::TrustAnchors::with_operator(&[key]).unwrap(),
        )
    }

    /// A one-entry state blob, so a test can name the state it expects a
    /// store to hold afterwards rather than an opaque byte string.
    fn state_blob(key: &str, value: &str) -> Vec<u8> {
        let source = MemoryStore::new();
        source
            .put("advertisements", key.as_bytes(), value.as_bytes())
            .unwrap();
        state::serialize(&source, &["advertisements"]).unwrap()
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
            locations: vec![
                SnapshotLocation::parse(format!("http://archive.example:7080/snapshot/{id}"))
                    .unwrap(),
            ],
            producer: peer_id_from_public_key(&provider.public_key()).unwrap(),
            producer_public_key: provider.public_key(),
        }
    }

    /// Announces `metadata` through the real signed path, so no test
    /// reaches `import` with an announcement the index never verified.
    fn seed(index: &SnapshotIndex<TestStore>, provider: &Keypair, metadata: &SnapshotMetadata) {
        index
            .apply_announce(SignedSnapshotAnnounce::sign(metadata.clone(), provider))
            .unwrap();
    }

    #[test]
    fn an_unregistered_announcer_is_rejected() {
        let services = Rc::new(Registry::new(Rc::new(MemoryStore::new())));
        let index = SnapshotIndex::new(Rc::new(MemoryStore::new()), services);
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
        let index = trusting(Rc::new(MemoryStore::new()), services, &provider);
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
        let index = trusting(Rc::new(MemoryStore::new()), services, &provider);
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
    fn importing_a_valid_snapshot_writes_the_state_and_advances_the_checkpoint() {
        let (provider, services) = registered_provider(1);
        let store = Rc::new(MemoryStore::new());
        let index = trusting(Rc::clone(&store), services, &provider);
        let blob = state_blob("ad-1", "the full marketplace state");
        let metadata = announce(&provider, "snap-1", 4217, &blob);
        seed(&index, &provider, &metadata);

        assert_eq!(index.import(&metadata.id, &blob).unwrap(), 1);
        assert_eq!(
            store.get("advertisements", b"ad-1").unwrap(),
            Some(b"the full marketplace state".to_vec()),
            "import must land the state, not merely return it"
        );
        assert_eq!(index.checkpoint_height(), Some(4217));
    }

    #[test]
    fn a_tampered_state_blob_fails_state_root_verification() {
        let (provider, services) = registered_provider(1);
        let store = Rc::new(MemoryStore::new());
        let index = trusting(Rc::clone(&store), services, &provider);
        let blob = state_blob("ad-1", "the real state");
        let metadata = announce(&provider, "snap-1", 100, &blob);
        seed(&index, &provider, &metadata);

        // One flipped bit, same length — so this reaches the digest check
        // rather than being caught by the size check first.
        let mut corrupted = blob.clone();
        let last = corrupted.len() - 1;
        corrupted[last] ^= 0x01;

        assert_eq!(
            index.import(&metadata.id, &corrupted),
            Err(SnapshotError::StateRootMismatch)
        );
        assert_eq!(index.checkpoint_height(), None);
        assert_eq!(
            store.get("advertisements", b"ad-1").unwrap(),
            None,
            "a rejected snapshot must leave no trace in the store"
        );
    }

    #[test]
    fn a_truncated_download_fails_the_size_check() {
        let (provider, services) = registered_provider(1);
        let index = trusting(Rc::new(MemoryStore::new()), services, &provider);
        let blob = state_blob("ad-1", "the full state");
        let metadata = announce(&provider, "snap-1", 100, &blob);
        seed(&index, &provider, &metadata);

        assert_eq!(
            index.import(&metadata.id, &blob[..blob.len() - 2]),
            Err(SnapshotError::SizeMismatch)
        );
    }

    /// The vector this closes: metadata the caller made up, whose
    /// `state_root` matches bytes the same caller supplied. It verifies
    /// perfectly and proves nothing, so `import` refuses to look at any
    /// announcement it did not itself verify.
    #[test]
    fn importing_a_snapshot_this_node_never_verified_is_refused() {
        let (_, services) = registered_provider(1);
        let index = SnapshotIndex::new(Rc::new(MemoryStore::new()), services);
        let stranger = Keypair::generate();
        let blob = state_blob("ad-1", "fabricated state");
        let metadata = announce(&stranger, "snap-forged", 9_000, &blob);

        assert_eq!(
            index.import(&metadata.id, &blob),
            Err(SnapshotError::UnknownSnapshot)
        );
        assert_eq!(index.checkpoint_height(), None);
    }

    /// A registration can lapse between announcing and importing, and by
    /// then the announcement's signature says nothing about whether the
    /// cluster still vouches for its producer.
    #[test]
    fn a_producer_deregistered_since_announcing_can_no_longer_be_imported_from() {
        let (provider, services) = registered_provider(1);
        let index = SnapshotIndex::new(Rc::new(MemoryStore::new()), Rc::clone(&services));
        let blob = state_blob("ad-1", "state");
        let metadata = announce(&provider, "snap-1", 100, &blob);
        seed(&index, &provider, &metadata);

        services.expire_stale(std::time::Duration::ZERO);
        assert_eq!(
            index.import(&metadata.id, &blob),
            Err(SnapshotError::Unauthorized)
        );
    }

    /// A brand-new cluster's first snapshot is taken at gossip event
    /// count zero, and a joining node with nothing to lose must still be
    /// able to import it.
    #[test]
    fn a_fresh_node_can_import_a_height_zero_snapshot() {
        let (provider, services) = registered_provider(1);
        let store = Rc::new(MemoryStore::new());
        let index = trusting(Rc::clone(&store), services, &provider);
        let blob = state_blob("ad-1", "genesis state");
        let metadata = announce(&provider, "snap-genesis", 0, &blob);
        seed(&index, &provider, &metadata);

        assert_eq!(index.import(&metadata.id, &blob).unwrap(), 1);
        assert_eq!(index.checkpoint_height(), Some(0));
    }

    /// Import now *writes* state, so replaying an older snapshot would
    /// roll this node backwards rather than merely leaving the checkpoint
    /// alone.
    #[test]
    fn importing_an_older_snapshot_is_refused_outright() {
        let (provider, services) = registered_provider(1);
        let store = Rc::new(MemoryStore::new());
        let index = trusting(Rc::clone(&store), services, &provider);

        let newer_blob = state_blob("ad-1", "newer state");
        let newer = announce(&provider, "snap-2", 500, &newer_blob);
        seed(&index, &provider, &newer);
        index.import(&newer.id, &newer_blob).unwrap();

        let older_blob = state_blob("ad-1", "older state");
        let older = announce(&provider, "snap-1", 100, &older_blob);
        seed(&index, &provider, &older);

        assert_eq!(
            index.import(&older.id, &older_blob),
            Err(SnapshotError::StaleSnapshot)
        );
        assert_eq!(index.checkpoint_height(), Some(500));
        assert_eq!(
            store.get("advertisements", b"ad-1").unwrap(),
            Some(b"newer state".to_vec())
        );
    }
}
