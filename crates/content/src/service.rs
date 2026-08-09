//! Drives one node's attachment index: applies incoming gossip events
//! automatically and provides the operation that originates new ones.

use crate::error::ContentError;
use crate::events::SignedAttachmentPublish;
use crate::protocol;
use crate::record::{Attachment, AttachmentId, AttachmentSubject, MediaType};
use crate::store::AttachmentRegistry;
use openfiat_crypto::Cid;
use openfiat_gossip::GossipService;
use openfiat_serialization::wire;
use openfiat_settlement::SettlementId;
use openfiat_storage::KvStore;
use openfiat_types::{EventType, PeerId, Priority, Timestamp};
use std::rc::Rc;

pub struct AttachmentService<S> {
    pub gossip: GossipService<S>,
    registry: Rc<AttachmentRegistry<S>>,
}

impl<S: KvStore + 'static> AttachmentService<S> {
    pub fn new(mut gossip: GossipService<S>, store: S) -> Self {
        let registry = Rc::new(AttachmentRegistry::new(store));
        let handler_registry = Rc::clone(&registry);
        gossip.add_event_handler(move |event| handler_registry.apply_event(event));
        Self { gossip, registry }
    }

    pub fn registry(&self) -> Rc<AttachmentRegistry<S>> {
        Rc::clone(&self.registry)
    }

    pub fn get(&self, id: &AttachmentId) -> Option<Attachment> {
        self.registry.get(id)
    }

    /// See [`AttachmentRegistry::find_by_settlement`] — `parties` is
    /// required, and for the same reason.
    pub fn find_by_settlement(
        &self,
        settlement: &SettlementId,
        parties: &[PeerId],
    ) -> Vec<Attachment> {
        self.registry.find_by_settlement(settlement, parties)
    }

    /// Publishes a reference to content already pinned elsewhere.
    ///
    /// `cid` is a [`Cid`], not a string: whoever uploaded the file is
    /// responsible for producing a valid identifier, and by the time it
    /// reaches here it has been parsed. This service never uploads and
    /// never holds a pinning credential.
    #[allow(clippy::too_many_arguments)]
    pub fn publish(
        &mut self,
        id: impl Into<String>,
        settlement: SettlementId,
        cid: Cid,
        media_type: MediaType,
        size_bytes: u64,
        caption: impl Into<String>,
    ) -> Result<AttachmentId, ContentError> {
        let attachment = Attachment {
            id: AttachmentId::new(id),
            subject: AttachmentSubject::Settlement(settlement),
            author: self.gossip.node.local_peer_id(),
            author_public_key: self.gossip.public_key(),
            cid,
            media_type,
            size_bytes,
            caption: caption.into(),
            created_at: Timestamp::now(),
        };
        attachment.validate()?;

        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::ATTACHMENT_PUBLISH,
            &attachment,
        )
        .map_err(|_| ContentError::MalformedAttachment)?;
        let signed = SignedAttachmentPublish {
            signature: self.gossip.sign(&bytes),
            attachment,
        };
        // Applied locally first, so a record this node would refuse from a
        // peer is not one it will ask peers to accept.
        let id = self.registry.apply_publish(signed.clone())?;

        let payload = wire::to_bytes(&signed).map_err(|_| ContentError::MalformedAttachment)?;
        let event_type = EventType::new(protocol::EVENT_PUBLISHED)
            .expect("AttachmentPublished is valid PascalCase");
        self.gossip
            .originate(
                event_type,
                protocol::OFS_SPEC,
                Priority::Reputation,
                8,
                payload,
            )
            .map_err(|_| ContentError::MalformedAttachment)?;
        Ok(id)
    }

    pub async fn drive_once(&mut self) {
        self.gossip.drive_once().await;
    }
}
