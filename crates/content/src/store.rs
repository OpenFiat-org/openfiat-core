//! The replicated local attachment index.
//!
//! # Why authorization happens on read, not on write
//!
//! Anyone can sign a record naming any settlement, so an attachment list
//! that showed every record naming a settlement would let a stranger drop
//! forged "evidence" into someone else's trade. Only the buyer and the
//! seller may contribute, and that has to be enforced somewhere.
//!
//! It is enforced in [`AttachmentRegistry::find_by_settlement`] rather
//! than in [`AttachmentRegistry::apply_publish`], because gossip has no
//! ordering guarantee. A node can receive an attachment before it has
//! received the settlement the attachment refers to — the author had it,
//! this node does not yet — and a write-time party check would see an
//! unknown settlement, conclude the author is not a party, and discard a
//! genuine record permanently. Evidence that survives or vanishes
//! depending on packet arrival order is worse than storing some records
//! that turn out to be unauthorized, because a discarded event is never
//! retried while an unauthorized record is simply never returned to
//! anyone.
//!
//! So [`apply_publish`] enforces everything checkable from the record
//! alone — signature, authorship, shape — and [`find_by_settlement`]
//! enforces the one fact that needs state. A caller cannot accidentally
//! skip the second check: there is no method that returns a settlement's
//! attachments without being given its parties.
//!
//! [`apply_publish`]: AttachmentRegistry::apply_publish
//! [`find_by_settlement`]: AttachmentRegistry::find_by_settlement

use crate::error::ContentError;
use crate::events::SignedAttachmentPublish;
use crate::protocol;
use crate::record::{Attachment, AttachmentId, AttachmentSubject};
use openfiat_serialization::wire;
use openfiat_settlement::SettlementId;
use openfiat_storage::KvStore;
use openfiat_types::{EventEnvelope, PeerId};

const COLUMN_FAMILY: &str = "attachments";

pub struct AttachmentRegistry<S> {
    store: S,
}

impl<S: KvStore> AttachmentRegistry<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }

    pub fn get(&self, id: &AttachmentId) -> Option<Attachment> {
        let bytes = self
            .store
            .get(COLUMN_FAMILY, id.as_str().as_bytes())
            .ok()
            .flatten()?;
        wire::from_bytes(&bytes).ok()
    }

    fn put(&self, attachment: &Attachment) {
        if let Ok(bytes) = wire::to_bytes(attachment) {
            let _ = self
                .store
                .put(COLUMN_FAMILY, attachment.id.as_str().as_bytes(), &bytes);
        }
    }

    pub fn all(&self) -> Vec<Attachment> {
        self.store
            .iter_prefix(COLUMN_FAMILY, &[])
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(_, value)| wire::from_bytes(&value).ok())
            .collect()
    }

    /// Every attachment on `settlement` that one of its parties published.
    ///
    /// `parties` is the buyer and seller from the settlement record. It is
    /// a required argument rather than something this registry looks up so
    /// that the authorization check cannot be forgotten — see the module
    /// documentation.
    ///
    /// Ordered oldest first, then by id, so two nodes holding the same
    /// records display them in the same order.
    pub fn find_by_settlement(
        &self,
        settlement: &SettlementId,
        parties: &[PeerId],
    ) -> Vec<Attachment> {
        let mut found: Vec<Attachment> = self
            .all()
            .into_iter()
            .filter(|a| {
                let AttachmentSubject::Settlement(id) = &a.subject;
                id == settlement && parties.contains(&a.author)
            })
            .collect();
        found.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        found
    }

    /// Stores a signed publication after every check that needs no state.
    pub fn apply_publish(
        &self,
        signed: SignedAttachmentPublish,
    ) -> Result<AttachmentId, ContentError> {
        signed.verify()?;
        signed.attachment.validate()?;
        let id = signed.attachment.id.clone();
        // Ids are author-chosen, so first-writer-wins here is what stops a
        // later record from replacing an earlier one under the same id —
        // the immutability `record` promises has to be enforced, not just
        // documented.
        if self.get(&id).is_some() {
            return Err(ContentError::DuplicateAttachmentId);
        }
        self.put(&signed.attachment);
        Ok(id)
    }

    pub fn apply_event(&self, event: &EventEnvelope) {
        if event.ofs_spec != protocol::OFS_SPEC
            || event.event_type.as_str() != protocol::EVENT_PUBLISHED
        {
            return;
        }
        if let Ok(signed) = wire::from_bytes(&event.payload) {
            let _ = self.apply_publish(signed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;
    use crate::record::MediaType;
    use openfiat_crypto::Keypair;
    use openfiat_network::identity::peer_id_from_public_key;
    use openfiat_storage::mem::MemoryStore;
    use openfiat_types::Timestamp;

    fn publish(
        registry: &AttachmentRegistry<MemoryStore>,
        author: &Keypair,
        id: &str,
        settlement: &str,
        at_millis: u64,
    ) -> Result<AttachmentId, ContentError> {
        let attachment = Attachment {
            id: AttachmentId::new(id),
            subject: AttachmentSubject::Settlement(SettlementId::new(settlement)),
            author: peer_id_from_public_key(&author.public_key()).unwrap(),
            author_public_key: author.public_key(),
            cid: fixtures::probe_cid(),
            media_type: MediaType::Png,
            size_bytes: 2_048,
            caption: "receipt".to_string(),
            created_at: Timestamp::from_millis(at_millis),
        };
        registry.apply_publish(SignedAttachmentPublish::sign(attachment, author))
    }

    fn peer(keypair: &Keypair) -> PeerId {
        peer_id_from_public_key(&keypair.public_key()).unwrap()
    }

    #[test]
    fn a_party_can_publish_and_read_back_their_attachment() {
        let registry = AttachmentRegistry::new(MemoryStore::new());
        let buyer = Keypair::generate();
        let seller = Keypair::generate();
        publish(&registry, &buyer, "att-1", "s-1", 10).unwrap();

        let found =
            registry.find_by_settlement(&SettlementId::new("s-1"), &[peer(&buyer), peer(&seller)]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].cid.as_str(), fixtures::PROBE_CID);
    }

    #[test]
    fn a_stranger_cannot_put_evidence_into_someone_elses_trade() {
        let registry = AttachmentRegistry::new(MemoryStore::new());
        let buyer = Keypair::generate();
        let seller = Keypair::generate();
        let stranger = Keypair::generate();

        publish(&registry, &buyer, "att-1", "s-1", 10).unwrap();
        // Perfectly well-signed, genuinely the stranger's own record. The
        // signature was never the question.
        publish(&registry, &stranger, "att-2", "s-1", 11).unwrap();

        let found =
            registry.find_by_settlement(&SettlementId::new("s-1"), &[peer(&buyer), peer(&seller)]);
        assert_eq!(found.len(), 1, "only the parties' attachments are returned");
        assert_eq!(found[0].author, peer(&buyer));
    }

    #[test]
    fn an_attachment_that_arrived_before_its_settlement_is_not_lost() {
        // The ordering hazard the module documentation describes: this
        // node stores the record while knowing nothing about `s-1`, and
        // only later learns who its parties are.
        let registry = AttachmentRegistry::new(MemoryStore::new());
        let seller = Keypair::generate();
        publish(&registry, &seller, "att-1", "s-1", 10).unwrap();

        let before_settlement_known = registry.find_by_settlement(&SettlementId::new("s-1"), &[]);
        assert!(before_settlement_known.is_empty());

        let now_known = registry.find_by_settlement(&SettlementId::new("s-1"), &[peer(&seller)]);
        assert_eq!(
            now_known.len(),
            1,
            "a genuine attachment must survive arriving out of order"
        );
    }

    #[test]
    fn attachments_on_another_settlement_are_not_mixed_in() {
        let registry = AttachmentRegistry::new(MemoryStore::new());
        let buyer = Keypair::generate();
        publish(&registry, &buyer, "att-1", "s-1", 10).unwrap();
        publish(&registry, &buyer, "att-2", "s-2", 11).unwrap();

        let found = registry.find_by_settlement(&SettlementId::new("s-1"), &[peer(&buyer)]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id.as_str(), "att-1");
    }

    #[test]
    fn a_second_record_cannot_overwrite_an_id_already_published() {
        let registry = AttachmentRegistry::new(MemoryStore::new());
        let buyer = Keypair::generate();
        publish(&registry, &buyer, "att-1", "s-1", 10).unwrap();
        assert_eq!(
            publish(&registry, &buyer, "att-1", "s-1", 20),
            Err(ContentError::DuplicateAttachmentId),
            "immutability has to be enforced, not merely documented"
        );
        assert_eq!(
            registry
                .get(&AttachmentId::new("att-1"))
                .unwrap()
                .created_at,
            Timestamp::from_millis(10)
        );
    }

    #[test]
    fn the_order_is_the_same_on_every_node() {
        let registry = AttachmentRegistry::new(MemoryStore::new());
        let buyer = Keypair::generate();
        publish(&registry, &buyer, "zzz", "s-1", 30).unwrap();
        publish(&registry, &buyer, "aaa", "s-1", 10).unwrap();
        publish(&registry, &buyer, "mmm", "s-1", 20).unwrap();

        let ids: Vec<_> = registry
            .find_by_settlement(&SettlementId::new("s-1"), &[peer(&buyer)])
            .into_iter()
            .map(|a| a.id.as_str().to_string())
            .collect();
        assert_eq!(ids, vec!["aaa", "mmm", "zzz"]);
    }

    #[test]
    fn a_forged_signature_never_reaches_the_store() {
        let registry = AttachmentRegistry::new(MemoryStore::new());
        let author = Keypair::generate();
        let impostor = Keypair::generate();
        let attachment = Attachment {
            id: AttachmentId::new("att-1"),
            subject: AttachmentSubject::Settlement(SettlementId::new("s-1")),
            author: peer(&author),
            author_public_key: author.public_key(),
            cid: fixtures::probe_cid(),
            media_type: MediaType::Png,
            size_bytes: 2_048,
            caption: "receipt".to_string(),
            created_at: Timestamp::from_millis(1),
        };
        let forged = SignedAttachmentPublish::sign(attachment, &impostor);
        assert_eq!(
            registry.apply_publish(forged),
            Err(ContentError::InvalidSignature)
        );
        assert!(registry.get(&AttachmentId::new("att-1")).is_none());
    }

    #[test]
    fn a_gossiped_event_from_another_spec_is_ignored() {
        let registry = AttachmentRegistry::new(MemoryStore::new());
        let author = Keypair::generate();
        let attachment = Attachment {
            id: AttachmentId::new("att-1"),
            subject: AttachmentSubject::Settlement(SettlementId::new("s-1")),
            author: peer(&author),
            author_public_key: author.public_key(),
            cid: fixtures::probe_cid(),
            media_type: MediaType::Png,
            size_bytes: 2_048,
            caption: String::new(),
            created_at: Timestamp::from_millis(1),
        };
        let payload = wire::to_bytes(&SignedAttachmentPublish::sign(attachment, &author)).unwrap();

        let mut envelope = EventEnvelope {
            id: openfiat_types::EventId::from_bytes([7; 32]),
            event_type: openfiat_types::EventType::new(protocol::EVENT_PUBLISHED).unwrap(),
            ofs_spec: 9999,
            version: 1,
            origin: peer(&author),
            timestamp: Timestamp::from_millis(1),
            ttl: 8,
            priority: openfiat_types::Priority::Reputation,
            signature: openfiat_types::Signature::from_bytes([0u8; 64]),
            payload,
        };
        registry.apply_event(&envelope);
        assert!(registry.get(&AttachmentId::new("att-1")).is_none());

        envelope.ofs_spec = protocol::OFS_SPEC;
        registry.apply_event(&envelope);
        assert!(registry.get(&AttachmentId::new("att-1")).is_some());
    }
}
