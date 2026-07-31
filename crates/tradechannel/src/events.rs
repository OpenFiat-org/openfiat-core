//! The two signed events a trade channel is made of.
//!
//! Neither event carries the signer's public key, which is a deliberate
//! departure from `SettlementInitiate` and `DisputeOpen`. Those two
//! *establish* an identity, so they must state the key they are claiming.
//! These two act on a settlement that already recorded both parties'
//! keys, so the key is looked up from that verified record instead — the
//! same shape `SettlementRegistry::apply_payment_submitted` uses. A
//! signer who supplied their own key here could sign as anyone whose peer
//! id they were willing to claim.

use crate::key::ChannelCiphertext;
use crate::record::EntryKind;
use openfiat_crypto::SealedBox;
use openfiat_settlement::SettlementId;
use openfiat_types::{PeerId, Signature, Timestamp};

/// One party handing another peer the channel key.
///
/// `role` is absent on purpose: the registry decides it from the
/// settlement and the dispute record, both of which it can check. See
/// [`crate::record::GrantRole`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TradeChannelKeyGrant {
    pub settlement_id: SettlementId,
    pub granter: PeerId,
    pub recipient: PeerId,
    pub key_id: crate::key::ChannelKeyId,
    pub sealed_key: SealedBox,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SignedTradeChannelKeyGrant {
    pub grant: TradeChannelKeyGrant,
    pub signature: Signature,
}

impl SignedTradeChannelKeyGrant {
    pub fn sign(grant: TradeChannelKeyGrant, keypair: &openfiat_crypto::Keypair) -> Self {
        let bytes = openfiat_serialization::json::to_bytes(&grant)
            .expect("TradeChannelKeyGrant always serializes");
        Self {
            signature: keypair.sign(&bytes),
            grant,
        }
    }
}

/// One party writing an entry into the channel.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TradeChannelEntryPost {
    pub settlement_id: SettlementId,
    pub author: PeerId,
    pub sequence: u64,
    pub kind: EntryKind,
    pub payload: ChannelCiphertext,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SignedTradeChannelEntryPost {
    pub post: TradeChannelEntryPost,
    pub signature: Signature,
}

impl SignedTradeChannelEntryPost {
    pub fn sign(post: TradeChannelEntryPost, keypair: &openfiat_crypto::Keypair) -> Self {
        let bytes = openfiat_serialization::json::to_bytes(&post)
            .expect("TradeChannelEntryPost always serializes");
        Self {
            signature: keypair.sign(&bytes),
            post,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key::{ChannelKey, EntryBinding, seal_entry};
    use openfiat_crypto::{Keypair, seal, verify};
    use openfiat_network::identity::peer_id_from_public_key;

    fn peer(keypair: &Keypair) -> PeerId {
        peer_id_from_public_key(&keypair.public_key()).unwrap()
    }

    /// The signature is taken over the serialized struct, so every field
    /// is covered without anyone having to remember to add it to a
    /// hand-written transcript. These two tests pin that for the fields
    /// that would be worth tampering with — the sealed key and the
    /// payload — so a future field cannot quietly fall outside it.
    #[test]
    fn swapping_the_sealed_key_invalidates_the_grants_signature() {
        let seller = Keypair::generate();
        let arbitrator = Keypair::generate();
        let attacker = Keypair::generate();
        let key = ChannelKey::generate();

        let grant = TradeChannelKeyGrant {
            settlement_id: SettlementId::new("settle-1"),
            granter: peer(&seller),
            recipient: peer(&arbitrator),
            key_id: key.id(),
            sealed_key: seal(&arbitrator.public_key(), key.expose()).unwrap(),
            timestamp: Timestamp::now(),
        };
        let mut signed = SignedTradeChannelKeyGrant::sign(grant, &seller);
        signed.grant.sealed_key = seal(&attacker.public_key(), key.expose()).unwrap();

        let bytes = openfiat_serialization::json::to_bytes(&signed.grant).unwrap();
        assert!(verify(&seller.public_key(), &bytes, &signed.signature).is_err());
    }

    #[test]
    fn swapping_the_payload_invalidates_the_posts_signature() {
        let seller = Keypair::generate();
        let key = ChannelKey::generate();
        let settlement_id = SettlementId::new("settle-1");
        let author = peer(&seller);
        let binding = EntryBinding {
            settlement_id: &settlement_id,
            author: &author,
            sequence: 0,
            kind: EntryKind::PaymentDetails.name(),
        };

        let post = TradeChannelEntryPost {
            settlement_id: settlement_id.clone(),
            author: author.clone(),
            sequence: 0,
            kind: EntryKind::PaymentDetails,
            payload: seal_entry(&key, &binding, b"account 0110123456789").unwrap(),
            timestamp: Timestamp::now(),
        };
        let mut signed = SignedTradeChannelEntryPost::sign(post, &seller);
        signed.post.payload = seal_entry(&key, &binding, b"account 9999999999999").unwrap();

        let bytes = openfiat_serialization::json::to_bytes(&signed.post).unwrap();
        assert!(verify(&seller.public_key(), &bytes, &signed.signature).is_err());
    }
}
