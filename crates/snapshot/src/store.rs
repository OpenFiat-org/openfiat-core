//! The replicated local snapshot index, sharing a handle to the node's
//! service registry (§5/§24: only a registered snapshot provider may
//! announce one), plus the import pipeline (§16-17) and this node's own
//! local checkpoint bookkeeping (§9, §18).

use crate::codec;
use crate::error::SnapshotError;
use crate::events::SignedSnapshotAnnounce;
use crate::protocol;
use crate::record::{SnapshotId, SnapshotMetadata};
use crate::stake::{ProviderStakes, StakeStanding};
use crate::state::EntryVerifier;
use crate::trust::TrustAnchors;
use openfiat_registry::Registry;
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{EventEnvelope, InfrastructureService, ServiceType, Timestamp};
use std::cell::RefCell;
use std::rc::Rc;

const SNAPSHOTS_COLUMN_FAMILY: &str = "snapshot_metadata";
const CHECKPOINT_COLUMN_FAMILY: &str = "snapshot_checkpoint";
const CHECKPOINT_KEY: &[u8] = b"local";

/// The column families an imported snapshot may never write — this
/// node's own snapshot bookkeeping. See `crate::state::restore` for the
/// lockout this prevents.
pub const RESERVED_COLUMN_FAMILIES: &[&str] = &[SNAPSHOTS_COLUMN_FAMILY, CHECKPOINT_COLUMN_FAMILY];

/// What makes one announcement newer than another from the same
/// producer, for [`SnapshotIndex::prune_superseded`].
///
/// Slot first, because that is what `latest` and the bootstrap ordering
/// already sort by, and a sweep that disagreed with them would delete the
/// announcement a joining node was about to fetch. Creation time breaks a
/// tie: a producer restarted onto the same slot announces twice, and one
/// of the two has to be the survivor rather than both or neither.
fn recency(metadata: &SnapshotMetadata) -> (u64, u64) {
    (metadata.slot, metadata.created_at.as_millis())
}

pub struct SnapshotIndex<S> {
    store: S,
    services: Rc<Registry<S>>,
    /// Who this node will take a *first* snapshot from, when it has no
    /// checkpoint of its own to judge one against.
    anchors: TrustAnchors,
    /// What this node will accept into each column family, entry by
    /// entry — see [`crate::state::EntryVerifier`].
    ///
    /// Held on the index rather than passed to `import`, so that every
    /// path into the store goes through it. A parameter would be a
    /// parameter some future call site passes `accept_any` to, and the
    /// one place that must never happen is the one that writes a peer's
    /// bytes into this node's content store.
    verify_entry: EntryVerifier,
    /// What this node has read from chain about who is actually staked as
    /// a snapshot provider — see [`crate::stake`].
    ///
    /// Shared rather than owned, because the poll loop that reads the
    /// chain lives outside this crate and must write into the same record
    /// this index reads. A private copy is how the announce-time answer
    /// and the import-time answer come to disagree.
    stakes: Rc<RefCell<ProviderStakes>>,
}

impl<S: KvStore> SnapshotIndex<S> {
    /// `verify_entry` is stated rather than defaulted: a default would be
    /// `accept_any`, and a node that composes a self-verifying column
    /// family and forgets to say so imports whatever a producer put in
    /// it. Pass `openfiat_snapshot::state::accept_any` when the answer is
    /// genuinely that there is nothing to check.
    pub fn new(store: S, services: Rc<Registry<S>>, verify_entry: EntryVerifier) -> Self {
        Self::with_anchors(store, services, TrustAnchors::pinned(), verify_entry)
    }

    /// The pinned anchors plus whatever the operator added — see
    /// `crate::trust`. Separate from `new` so the common case cannot
    /// forget them: `new` is anchored too, and there is no constructor
    /// that produces an unanchored index.
    pub fn with_anchors(
        store: S,
        services: Rc<Registry<S>>,
        anchors: TrustAnchors,
        verify_entry: EntryVerifier,
    ) -> Self {
        Self {
            store,
            services,
            anchors,
            verify_entry,
            // Unenforceable by default, and unlike `verify_entry` a
            // default is safe here precisely because it is the strictest
            // setting: a node that has not been told it can read the
            // chain refuses to import from anyone outside its anchors.
            // Getting this wrong fails closed.
            stakes: Rc::new(RefCell::new(ProviderStakes::unenforceable())),
        }
    }

    /// Read this node's stake record from the same handle the chain poll
    /// loop writes to.
    ///
    /// Consumed at construction rather than at each `import`, so that
    /// there is exactly one record and no call site can supply a
    /// permissive one of its own — the same argument `verify_entry` makes
    /// for living on the index.
    pub fn with_stakes(mut self, stakes: Rc<RefCell<ProviderStakes>>) -> Self {
        self.stakes = stakes;
        self
    }

    /// This node's stake record, for the poll loop that maintains it and
    /// for diagnostics.
    pub fn stakes(&self) -> &Rc<RefCell<ProviderStakes>> {
        &self.stakes
    }

    /// Where `producer` stands against [`crate::stake::MINIMUM_PROVIDER_STAKE`]
    /// as of `now`.
    pub fn stake_standing(
        &self,
        producer: &openfiat_types::PeerId,
        now: Timestamp,
    ) -> StakeStanding {
        self.stakes.borrow().standing(producer, now)
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

    /// §13/§20: the highest-slot snapshot this node currently knows
    /// about, from any provider.
    pub fn latest(&self) -> Option<SnapshotMetadata> {
        self.all().into_iter().max_by_key(|metadata| metadata.slot)
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
    ///
    /// # Why the stake gate refuses only a *known* shortfall here
    ///
    /// An announcement is metadata. Holding one is harmless — nothing can
    /// be imported from it that `import` would not check again — and
    /// gossip never replays, so an announcement refused because this node
    /// had not yet polled the chain is one it will not hear again until
    /// the producer's next production interval. Refusing on "unread"
    /// would therefore trade real availability for no security at all.
    ///
    /// A shortfall this node has actually *observed* is a different
    /// matter: keeping and relaying that announcement means publishing a
    /// claim this node knows to be unbacked. So the gate here fires only
    /// on [`StakeStanding::Insufficient`], and the gate that protects the
    /// node's state lives at [`Self::import`].
    pub fn apply_announce(
        &self,
        signed: SignedSnapshotAnnounce,
    ) -> Result<SnapshotId, SnapshotError> {
        signed.verify()?;
        let metadata = signed.metadata;
        if !self.is_registered_provider(&metadata.producer) {
            return Err(SnapshotError::Unauthorized);
        }
        if let StakeStanding::Insufficient { .. } =
            self.stake_standing(&metadata.producer, Timestamp::now())
        {
            return Err(SnapshotError::InsufficientProviderStake);
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
    /// 3. **The producer must be a trust anchor, or hold a stake this
    ///    node has read from chain and found sufficient** — see
    ///    [`crate::stake`], which is the requirement `crate::trust` said
    ///    governs after the first snapshot and which, until now, did not
    ///    exist.
    /// 4. **The snapshot must be newer than what this node already has.**
    ///    Unlike the earlier read-only version of this method, importing
    ///    now *writes state*, so replaying an old snapshot would silently
    ///    roll the node backwards.
    /// 5. **Size, then digest, then write.** Nothing reaches the store
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
        // establishes what the correct state root at a slot IS. So the
        // first snapshot — and only the first — must come from a pinned
        // anchor. After this import the node has its own basis, and
        // registration governs. See `crate::trust`.
        if self.checkpoint_slot().is_none() && !self.anchors.trusts(&metadata.producer) {
            return Err(SnapshotError::UntrustedFirstSnapshot);
        }
        // And after the first, `crate::trust` promised that "the stake
        // requirement on registration governs instead". This is that
        // requirement, and until now it did not exist: registration is
        // free, so every check above passed for anyone willing to sign.
        //
        // An anchor is exempt because it holds the stronger credential.
        // Requiring a pinned key to also post a bond adds nothing —
        // whoever controls that key already decides what a bootstrapping
        // node believes — while removing the exemption would leave a node
        // whose RPC endpoint is momentarily down unable to bootstrap at
        // all, which is the one thing the anchors exist to guarantee.
        if !self.anchors.trusts(&metadata.producer) {
            match self.stake_standing(&metadata.producer, Timestamp::now()) {
                StakeStanding::Qualified => {}
                StakeStanding::Insufficient { .. } => {
                    return Err(SnapshotError::InsufficientProviderStake);
                }
                // Including `Unenforceable`, which is a `GossipOnly`
                // node's permanent answer: it cannot read a stake, so for
                // it the anchors govern every import and not merely the
                // first. Said plainly rather than waved through — see
                // `crate::stake`.
                StakeStanding::Unread | StakeStanding::Unenforceable => {
                    return Err(SnapshotError::ProviderStakeUnverified);
                }
            }
        }
        // Only a node that has already imported something can be rolled
        // backwards. A node with no checkpoint has no state to lose and
        // must be able to import *any* slot — including 0, which is
        // what a brand-new cluster's first snapshot legitimately carries.
        if self
            .checkpoint_slot()
            .is_some_and(|current| metadata.slot <= current)
        {
            return Err(SnapshotError::StaleSnapshot);
        }
        // Before the size check rather than after: `size_bytes` is a
        // number in somebody else's announcement, and the two failures
        // say different things. "Larger than this node will hold" is
        // about this node; "not the size announced" is about the bytes.
        if metadata.size_bytes > codec::MAX_SNAPSHOT_BYTES {
            return Err(SnapshotError::SnapshotTooLarge);
        }
        if compressed_bytes.len() as u64 != metadata.size_bytes {
            return Err(SnapshotError::SizeMismatch);
        }
        let state_bytes = codec::decompress(compressed_bytes, metadata.compression)?;
        if codec::state_root(&state_bytes) != metadata.state_root {
            return Err(SnapshotError::StateRootMismatch);
        }

        let restored = crate::state::restore(
            &self.store,
            &state_bytes,
            RESERVED_COLUMN_FAMILIES,
            self.verify_entry,
        )?;
        // Last, and only once the state it describes is actually on disk:
        // a checkpoint advanced ahead of the state would tell this node's
        // gossip catch-up to resume from a slot whose state it does not
        // have.
        self.store
            .put(
                CHECKPOINT_COLUMN_FAMILY,
                CHECKPOINT_KEY,
                &metadata.slot.to_le_bytes(),
            )
            .map_err(|_| SnapshotError::StateUnwritable)?;
        Ok(restored)
    }

    /// Drops every announcement superseded by a newer one from the same
    /// producer, returning how many were forgotten.
    ///
    /// # Why the index has to be swept at all
    ///
    /// `apply_announce` writes every announcement it accepts and nothing
    /// ever removed one, so this column family grew by one entry per
    /// producer per production interval, for the life of the node — the
    /// only thing here that grows purely with time. `all()` deserializes
    /// the lot on every `getSnapshots` call and on every bootstrap tick,
    /// so it is not merely disk: it is a scan that gets slower forever.
    ///
    /// # Why the newest per producer is the right thing to keep
    ///
    /// A producer keeps exactly one file on disk
    /// ([`crate::config::DEFAULT_RETAIN`]), so its second-newest
    /// announcement already names a file that has been deleted. Keeping
    /// that metadata does not preserve a fallback; it preserves a
    /// download that 404s. The real fallback is another *producer's*
    /// announcement, which this keeps — one per producer, however many
    /// producers there are — and which is exactly what
    /// `poll_snapshot_bootstrap` falls through to.
    ///
    /// Per producer rather than by age, because age would encode an
    /// assumption about cadence that no producer is obliged to honour: a
    /// node snapshotting weekly would have its only live announcement
    /// swept by any window shorter than a week.
    ///
    /// # §24 says snapshot ids are permanent, and this does not break it
    ///
    /// Forgetting an id means `apply_announce` would accept it again
    /// instead of refusing it as a duplicate. That buys nobody anything.
    /// A re-announcement still has to be signed by a producer the service
    /// registry vouches for, still has to name a slot above this node's
    /// checkpoint, and still has to ship bytes of the announced size that
    /// hash to the announced state root — every condition a *fresh* id
    /// would have to meet. The duplicate check is bookkeeping that stops
    /// a producer overwriting its own live announcement, not a security
    /// boundary.
    pub fn prune_superseded(&self) -> usize {
        let held = self.all();
        let mut newest: std::collections::HashMap<&openfiat_types::PeerId, &SnapshotMetadata> =
            std::collections::HashMap::new();
        for metadata in &held {
            let best = newest.entry(&metadata.producer).or_insert(metadata);
            if recency(metadata) > recency(best) {
                *best = metadata;
            }
        }

        let mut dropped = 0;
        for metadata in &held {
            let survives = newest
                .get(&metadata.producer)
                .is_some_and(|best| best.id == metadata.id);
            if !survives
                && self
                    .store
                    .delete(SNAPSHOTS_COLUMN_FAMILY, metadata.id.as_str().as_bytes())
                    .is_ok()
            {
                dropped += 1;
            }
        }
        dropped
    }

    /// §9/§18: the slot of the most recent snapshot this node has
    /// actually imported — where gossip catch-up replay should resume
    /// from, instead of full history replay.
    pub fn checkpoint_slot(&self) -> Option<u64> {
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
            branding: None,
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
    /// slot ordering — not about who is trusted, and a randomly seeded
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
            crate::state::accept_any,
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

    fn announce(provider: &Keypair, id: &str, slot: u64, state_bytes: &[u8]) -> SnapshotMetadata {
        SnapshotMetadata {
            id: SnapshotId::new(id),
            snapshot_version: 1,
            protocol_version: protocol::SUPPORTED_PROTOCOL_VERSION,
            slot,
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
        let index = SnapshotIndex::new(
            Rc::new(MemoryStore::new()),
            services,
            crate::state::accept_any,
        );
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
        assert_eq!(index.get(&id).unwrap().slot, 100);
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
        assert_eq!(index.latest().unwrap().slot, 250);
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
        assert_eq!(index.checkpoint_slot(), Some(4217));
    }

    #[test]
    fn an_entry_the_importer_refuses_stops_a_snapshot_that_verifies_perfectly() {
        // The state root is computed by the producer over whatever they
        // assembled, so a blob full of contents this node will not stand
        // behind passes every check above and must still be refused. The
        // announcement here is genuine: signed, registry-authorized,
        // anchored, and hashing to its own bytes.
        fn refuse_advertisements(family: &str, _key: &[u8], _value: &[u8]) -> bool {
            family != "advertisements"
        }

        let (provider, services) = registered_provider(1);
        let store = Rc::new(MemoryStore::new());
        let key = bs58::encode(provider.public_key().as_bytes()).into_string();
        let index = SnapshotIndex::with_anchors(
            Rc::clone(&store),
            services,
            crate::trust::TrustAnchors::with_operator(&[key]).unwrap(),
            refuse_advertisements,
        );
        let blob = state_blob("ad-1", "state this importer will not accept");
        let metadata = announce(&provider, "snap-1", 100, &blob);
        seed(&index, &provider, &metadata);

        assert_eq!(
            index.import(&metadata.id, &blob),
            Err(SnapshotError::UnverifiableEntry)
        );
        assert_eq!(store.get("advertisements", b"ad-1").unwrap(), None);
        assert_eq!(
            index.checkpoint_slot(),
            None,
            "a refused import must not advance the checkpoint past state \
             this node does not have"
        );
    }

    #[test]
    fn a_snapshot_larger_than_this_node_will_hold_is_refused_before_it_is_read() {
        // Snapshots carry content blocks now, so their size follows real
        // trading volume and somebody else's retention window. Without a
        // ceiling, an announcement is an instruction to allocate whatever
        // the producer says.
        let (provider, services) = registered_provider(1);
        let index = trusting(Rc::new(MemoryStore::new()), services, &provider);
        let blob = state_blob("ad-1", "small");
        let mut metadata = announce(&provider, "snap-1", 100, &blob);
        metadata.size_bytes = codec::MAX_SNAPSHOT_BYTES + 1;
        seed(&index, &provider, &metadata);

        assert_eq!(
            index.import(&metadata.id, &blob),
            Err(SnapshotError::SnapshotTooLarge)
        );
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
        assert_eq!(index.checkpoint_slot(), None);
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
        let index = SnapshotIndex::new(
            Rc::new(MemoryStore::new()),
            services,
            crate::state::accept_any,
        );
        let stranger = Keypair::generate();
        let blob = state_blob("ad-1", "fabricated state");
        let metadata = announce(&stranger, "snap-forged", 9_000, &blob);

        assert_eq!(
            index.import(&metadata.id, &blob),
            Err(SnapshotError::UnknownSnapshot)
        );
        assert_eq!(index.checkpoint_slot(), None);
    }

    /// A registration can lapse between announcing and importing, and by
    /// then the announcement's signature says nothing about whether the
    /// cluster still vouches for its producer.
    #[test]
    fn a_producer_deregistered_since_announcing_can_no_longer_be_imported_from() {
        let (provider, services) = registered_provider(1);
        let index = SnapshotIndex::new(
            Rc::new(MemoryStore::new()),
            Rc::clone(&services),
            crate::state::accept_any,
        );
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
        assert_eq!(index.checkpoint_slot(), Some(0));
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
        assert_eq!(index.checkpoint_slot(), Some(500));
        assert_eq!(
            store.get("advertisements", b"ad-1").unwrap(),
            Some(b"newer state".to_vec())
        );
    }

    /// A producer that has been running for a day has announced twenty-four
    /// snapshots and still holds one file. Without a sweep, every node that
    /// heard those announcements carries all twenty-four for ever, and
    /// re-reads them on every `getSnapshots` call and every bootstrap tick.
    #[test]
    fn a_producers_superseded_announcements_are_forgotten_and_its_newest_is_not() {
        let (provider, services) = registered_provider(1);
        let index = trusting(Rc::new(MemoryStore::new()), services, &provider);
        for (id, slot) in [("snap-1", 100), ("snap-2", 200), ("snap-3", 300)] {
            seed(&index, &provider, &announce(&provider, id, slot, b"state"));
        }

        assert_eq!(index.prune_superseded(), 2);
        let held = index.all();
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].id, SnapshotId::new("snap-3"));
        assert_eq!(
            index.latest().map(|m| m.slot),
            Some(300),
            "the announcement a joining node would actually fetch must survive"
        );
    }

    /// The reason the sweep is per producer rather than by age: another
    /// producer's snapshot is the fallback a joining node falls through to
    /// when the first one it tries is unreachable, and a sweep that kept
    /// only the single newest announcement in the index would delete every
    /// fallback the network has.
    #[test]
    fn every_producer_keeps_one_announcement_however_stale_it_is_beside_the_others() {
        let (fast, services) = registered_provider(1);
        let slow = Keypair::from_seed([2u8; 32]);
        services
            .apply_registration(SignedRegistration::sign(
                Registration {
                    service_id: ServiceId::new("snapshot-svc-slow"),
                    service_type: ServiceType::Infrastructure(
                        InfrastructureService::SnapshotProvider,
                    ),
                    provider: peer_id_from_public_key(&slow.public_key()).unwrap(),
                    provider_public_key: slow.public_key(),
                    endpoints: vec![],
                    supported_ofs: vec![1300],
                    region: None,
                    capabilities: vec![],
                    branding: None,
                    pricing: None,
                    payout_wallet: None,
                    timestamp: Timestamp::now(),
                },
                &slow,
            ))
            .unwrap();
        let index = trusting(Rc::new(MemoryStore::new()), services, &fast);

        seed(&index, &slow, &announce(&slow, "weekly-1", 10, b"state"));
        for (id, slot) in [("hourly-1", 100), ("hourly-2", 200)] {
            seed(&index, &fast, &announce(&fast, id, slot, b"state"));
        }

        assert_eq!(
            index.prune_superseded(),
            1,
            "only the superseded hourly one"
        );
        let mut ids: Vec<String> = index
            .all()
            .into_iter()
            .map(|m| m.id.as_str().to_string())
            .collect();
        ids.sort();
        assert_eq!(ids, ["hourly-2", "weekly-1"]);
    }

    /// Adds `provider` to an existing registry as a snapshot provider.
    fn also_register(services: &Registry<TestStore>, provider: &Keypair, service_id: &str) {
        services
            .apply_registration(SignedRegistration::sign(
                Registration {
                    service_id: ServiceId::new(service_id),
                    service_type: ServiceType::Infrastructure(
                        InfrastructureService::SnapshotProvider,
                    ),
                    provider: peer_id_from_public_key(&provider.public_key()).unwrap(),
                    provider_public_key: provider.public_key(),
                    endpoints: vec![],
                    supported_ofs: vec![1300],
                    region: None,
                    capabilities: vec![],
                    branding: None,
                    pricing: None,
                    payout_wallet: None,
                    timestamp: Timestamp::now(),
                },
                provider,
            ))
            .unwrap();
    }

    /// A node that has already bootstrapped from its anchor and now hears
    /// from a second, unanchored registered provider — the state every
    /// node is in a few minutes after it starts, and the one the stake
    /// gate exists for.
    ///
    /// Returns the index, its shared stake record, the stranger, and the
    /// stranger's newer snapshot.
    #[allow(clippy::type_complexity)]
    fn bootstrapped_node() -> (
        SnapshotIndex<TestStore>,
        TestStore,
        Rc<RefCell<crate::stake::ProviderStakes>>,
        Keypair,
        (SnapshotMetadata, Vec<u8>),
    ) {
        let (anchor, services) = registered_provider(1);
        let stranger = Keypair::from_seed([9u8; 32]);
        also_register(&services, &stranger, "snapshot-svc-stranger");

        let store = Rc::new(MemoryStore::new());
        let stakes = Rc::new(RefCell::new(crate::stake::ProviderStakes::enforcing()));
        let anchor_key = bs58::encode(anchor.public_key().as_bytes()).into_string();
        let index = SnapshotIndex::with_anchors(
            Rc::clone(&store),
            services,
            crate::trust::TrustAnchors::with_operator(&[anchor_key]).unwrap(),
            crate::state::accept_any,
        )
        .with_stakes(Rc::clone(&stakes));

        // A real first import from the anchor, so the node genuinely has
        // a checkpoint rather than a test-set one.
        let first = state_blob("ad-1", "the anchor's state");
        let metadata = announce(&anchor, "snap-anchor", 100, &first);
        seed(&index, &anchor, &metadata);
        index.import(&metadata.id, &first).unwrap();
        assert_eq!(index.checkpoint_slot(), Some(100));

        let blob = state_blob("ad-1", "a stranger's replacement worldview");
        let newer = announce(&stranger, "snap-stranger", 500, &blob);
        (index, store, stakes, stranger, (newer, blob))
    }

    /// The gate this whole module exists for. Every other check passes:
    /// the announcement is genuinely signed, the producer is genuinely a
    /// registered snapshot provider, the slot is genuinely newer, the
    /// bytes genuinely hash to the announced root. Registration is free,
    /// so without a stake requirement all of that costs a signature — and
    /// this import replaces the node's entire state with a stranger's.
    ///
    /// Remove the stake gate from `import` and this assertion fails with
    /// `Ok(1)`, and the assertion below it finds the stranger's state on
    /// disk.
    #[test]
    fn a_registered_provider_with_no_verified_stake_cannot_replace_this_nodes_worldview() {
        let (index, store, _stakes, stranger, (newer, blob)) = bootstrapped_node();
        seed(&index, &stranger, &newer);

        assert_eq!(
            index.import(&newer.id, &blob),
            Err(SnapshotError::ProviderStakeUnverified)
        );
        assert_eq!(index.checkpoint_slot(), Some(100));
        assert_eq!(
            store.get("advertisements", b"ad-1").unwrap(),
            Some(b"the anchor's state".to_vec()),
            "a refused snapshot must leave the node's own state untouched"
        );
    }

    /// And the same import once the chain has been read and the stake is
    /// there — so the test above is proving a gate rather than a pipeline
    /// that never worked.
    #[test]
    fn the_same_provider_is_imported_from_once_its_stake_has_been_read() {
        let (index, store, stakes, stranger, (newer, blob)) = bootstrapped_node();
        seed(&index, &stranger, &newer);
        stakes.borrow_mut().observe(
            peer_id_from_public_key(&stranger.public_key()).unwrap(),
            crate::stake::MINIMUM_PROVIDER_STAKE,
            Timestamp::now(),
        );

        assert_eq!(index.import(&newer.id, &blob).unwrap(), 1);
        assert_eq!(index.checkpoint_slot(), Some(500));
        assert_eq!(
            store.get("advertisements", b"ad-1").unwrap(),
            Some(b"a stranger's replacement worldview".to_vec())
        );
    }

    /// One base unit short. A minimum that accepted "nearly" would not be
    /// a minimum, and the boundary is where a rounding or unit error
    /// (whole OPEN against base units — a factor of a billion) would show
    /// up.
    #[test]
    fn a_provider_one_base_unit_short_of_the_minimum_is_refused() {
        let (index, _store, stakes, stranger, (newer, blob)) = bootstrapped_node();
        seed(&index, &stranger, &newer);
        stakes.borrow_mut().observe(
            peer_id_from_public_key(&stranger.public_key()).unwrap(),
            crate::stake::MINIMUM_PROVIDER_STAKE - 1,
            Timestamp::now(),
        );

        assert_eq!(
            index.import(&newer.id, &blob),
            Err(SnapshotError::InsufficientProviderStake)
        );
    }

    /// A provider that qualified when it announced and has since unstaked
    /// or been slashed. The signature on the announcement is still
    /// perfect, and says nothing about whether the bond is still posted —
    /// the same argument `is_registered_provider` already makes for
    /// re-checking registration at import rather than only at announce.
    #[test]
    fn a_provider_that_unstaked_after_announcing_can_no_longer_be_imported_from() {
        let (index, _store, stakes, stranger, (newer, blob)) = bootstrapped_node();
        let peer = peer_id_from_public_key(&stranger.public_key()).unwrap();
        stakes.borrow_mut().observe(
            peer.clone(),
            crate::stake::MINIMUM_PROVIDER_STAKE,
            Timestamp::now(),
        );
        seed(&index, &stranger, &newer);

        stakes.borrow_mut().observe(peer, 0, Timestamp::now());
        assert_eq!(
            index.import(&newer.id, &blob),
            Err(SnapshotError::InsufficientProviderStake)
        );
    }

    /// Announcements are metadata and cost nothing to hold, so an
    /// unread stake must not lose one — gossip does not replay. A
    /// shortfall this node has actually observed is different: keeping and
    /// serving that announcement onward publishes a claim it knows to be
    /// unbacked.
    #[test]
    fn an_unread_stake_keeps_an_announcement_and_an_observed_shortfall_drops_it() {
        let (index, _store, stakes, stranger, (newer, _blob)) = bootstrapped_node();
        let peer = peer_id_from_public_key(&stranger.public_key()).unwrap();

        assert!(
            index
                .apply_announce(SignedSnapshotAnnounce::sign(newer.clone(), &stranger))
                .is_ok(),
            "an announcement heard before the first stake poll must not be thrown away"
        );

        stakes.borrow_mut().observe(peer, 1, Timestamp::now());
        let another = announce(&stranger, "snap-stranger-2", 600, b"state");
        assert_eq!(
            index.apply_announce(SignedSnapshotAnnounce::sign(another, &stranger)),
            Err(SnapshotError::InsufficientProviderStake)
        );
    }

    /// The honest limit of this gate. A `GossipOnly` node has no RPC
    /// endpoint, so it can never read a `StakeAccount` — and rather than
    /// waving every registered provider through, it falls back to the one
    /// credential it can evaluate locally: its anchors, applied to every
    /// import instead of only the first. See `crate::stake`.
    #[test]
    fn a_gossip_only_node_imports_only_from_its_anchors_and_says_why() {
        let (anchor, services) = registered_provider(1);
        let stranger = Keypair::from_seed([9u8; 32]);
        also_register(&services, &stranger, "snapshot-svc-stranger");
        let store = Rc::new(MemoryStore::new());
        let index = trusting(Rc::clone(&store), services, &anchor);

        let first = state_blob("ad-1", "the anchor's state");
        let metadata = announce(&anchor, "snap-anchor", 100, &first);
        seed(&index, &anchor, &metadata);
        index.import(&metadata.id, &first).unwrap();

        let blob = state_blob("ad-1", "a stranger's replacement worldview");
        let newer = announce(&stranger, "snap-stranger", 500, &blob);
        seed(&index, &stranger, &newer);

        assert_eq!(
            index.import(&newer.id, &blob),
            Err(SnapshotError::ProviderStakeUnverified),
            "not `InsufficientProviderStake`: this node did not look, and cannot"
        );
        assert_eq!(
            index.stake_standing(&newer.producer, Timestamp::now()),
            crate::stake::StakeStanding::Unenforceable
        );
    }

    /// The anchors are exempt, and that is not an oversight: whoever holds
    /// a pinned key already decides what a bootstrapping node believes, so
    /// a bond under it adds nothing — while removing the exemption would
    /// leave a node whose RPC is momentarily down unable to bootstrap at
    /// all, which is the one thing the anchors exist to guarantee.
    #[test]
    fn an_anchor_needs_no_stake_because_it_holds_the_stronger_credential() {
        let (anchor, services) = registered_provider(1);
        let store = Rc::new(MemoryStore::new());
        let stakes = Rc::new(RefCell::new(crate::stake::ProviderStakes::enforcing()));
        let key = bs58::encode(anchor.public_key().as_bytes()).into_string();
        let index = SnapshotIndex::with_anchors(
            Rc::clone(&store),
            services,
            crate::trust::TrustAnchors::with_operator(&[key]).unwrap(),
            crate::state::accept_any,
        )
        .with_stakes(stakes);

        for (id, slot, value) in [("snap-1", 100, "first"), ("snap-2", 200, "second")] {
            let blob = state_blob("ad-1", value);
            let metadata = announce(&anchor, id, slot, &blob);
            seed(&index, &anchor, &metadata);
            index.import(&metadata.id, &blob).unwrap();
        }
        assert_eq!(index.checkpoint_slot(), Some(200));
    }

    #[test]
    fn sweeping_an_index_that_holds_nothing_superseded_changes_nothing() {
        let (provider, services) = registered_provider(1);
        let index = trusting(Rc::new(MemoryStore::new()), services, &provider);
        assert_eq!(index.prune_superseded(), 0, "an empty index");
        seed(&index, &provider, &announce(&provider, "snap-1", 100, b"s"));
        assert_eq!(index.prune_superseded(), 0);
        assert_eq!(index.prune_superseded(), 0, "and it is idempotent");
        assert_eq!(index.all().len(), 1);
    }
}
