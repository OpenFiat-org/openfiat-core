//! The signed publish event.
//!
//! One event, `AttachmentPublished`. There is no update and no delete —
//! see [`crate::record::Attachment`] for why an attachment a party could
//! retract would be worthless as evidence.

use crate::error::ContentError;
use crate::record::Attachment;
use openfiat_crypto::{Keypair, verify};
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_types::Signature;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedAttachmentPublish {
    pub attachment: Attachment,
    pub signature: Signature,
}

impl SignedAttachmentPublish {
    pub fn sign(attachment: Attachment, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::json::to_bytes(&attachment)
            .expect("Attachment always serializes");
        Self {
            signature: keypair.sign(&bytes),
            attachment,
        }
    }

    /// Self-consistency: the author's claimed [`PeerId`] must be derived
    /// from the public key travelling with the record, and the signature
    /// must be that key's.
    ///
    /// This proves who signed, and nothing more. Whether that signer is
    /// entitled to attach to this particular settlement needs the
    /// settlement, so it lives in [`crate::AttachmentRegistry`] — the same
    /// two-tier split every other signed action in this workspace uses.
    ///
    /// [`PeerId`]: openfiat_types::PeerId
    pub fn verify(&self) -> Result<(), ContentError> {
        let expected = peer_id_from_public_key(&self.attachment.author_public_key)
            .map_err(|_| ContentError::InvalidSignature)?;
        if expected != self.attachment.author {
            return Err(ContentError::NotAParty);
        }
        let bytes = openfiat_serialization::json::to_bytes(&self.attachment)
            .map_err(|_| ContentError::MalformedAttachment)?;
        verify(&self.attachment.author_public_key, &bytes, &self.signature)
            .map_err(|_| ContentError::InvalidSignature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{AttachmentId, AttachmentSubject, MediaType};
    use openfiat_settlement::SettlementId;
    use openfiat_types::Timestamp;

    fn attachment_for(keypair: &Keypair) -> Attachment {
        Attachment {
            id: AttachmentId::new("att-1"),
            subject: AttachmentSubject::Settlement(SettlementId::new("s-1")),
            author: peer_id_from_public_key(&keypair.public_key()).unwrap(),
            author_public_key: keypair.public_key(),
            cid: crate::fixtures::probe_cid(),
            media_type: MediaType::Png,
            size_bytes: 4_096,
            caption: "bank transfer receipt".to_string(),
            created_at: Timestamp::from_millis(1_000),
        }
    }

    #[test]
    fn a_genuine_publication_verifies() {
        let author = Keypair::generate();
        let signed = SignedAttachmentPublish::sign(attachment_for(&author), &author);
        assert_eq!(signed.verify(), Ok(()));
    }

    #[test]
    fn a_record_signed_by_someone_else_is_rejected() {
        let author = Keypair::generate();
        let impostor = Keypair::generate();
        let signed = SignedAttachmentPublish::sign(attachment_for(&author), &impostor);
        assert_eq!(signed.verify(), Err(ContentError::InvalidSignature));
    }

    #[test]
    fn a_key_that_is_not_the_claimed_author_is_rejected() {
        let author = Keypair::generate();
        let other = Keypair::generate();
        let mut attachment = attachment_for(&author);
        attachment.author_public_key = other.public_key();
        let signed = SignedAttachmentPublish::sign(attachment, &other);
        assert_eq!(
            signed.verify(),
            Err(ContentError::NotAParty),
            "the peer id must be derived from the key, not asserted beside it"
        );
    }

    #[test]
    fn swapping_the_cid_after_signing_invalidates_the_record() {
        let author = Keypair::generate();
        let mut signed = SignedAttachmentPublish::sign(attachment_for(&author), &author);
        signed.attachment.cid = crate::fixtures::other_cid();
        assert_eq!(
            signed.verify(),
            Err(ContentError::InvalidSignature),
            "pointing a signed record at different content must break it"
        );
    }
}
