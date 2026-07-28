//! Drives one node's identity index: applies incoming gossip events
//! automatically and provides the operations that originate new ones.

use crate::error::IdentityError;
use crate::events::{
    ClaimPublish, ClaimRevoke, ClaimVerify, SignedClaimPublish, SignedClaimRevoke,
    SignedClaimVerify,
};
use crate::protocol;
use crate::record::{Claim, ClaimId, ClaimType};
use crate::store::IdentityRegistry;
use openfiat_gossip::GossipService;
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{EventType, PeerId, Priority, Timestamp};
use std::rc::Rc;

pub struct IdentityService<S> {
    pub gossip: GossipService<S>,
    registry: Rc<IdentityRegistry<S>>,
}

impl<S: KvStore + 'static> IdentityService<S> {
    pub fn new(mut gossip: GossipService<S>, store: S) -> Self {
        let registry = Rc::new(IdentityRegistry::new(store));
        let handler_registry = Rc::clone(&registry);
        gossip.add_event_handler(move |event| handler_registry.apply_event(event));
        Self { gossip, registry }
    }

    pub fn registry(&self) -> Rc<IdentityRegistry<S>> {
        Rc::clone(&self.registry)
    }

    pub fn get(&self, id: &ClaimId) -> Option<Claim> {
        self.registry.get(id)
    }

    pub fn all(&self) -> Vec<Claim> {
        self.registry.all()
    }

    pub fn find_by_wallet(&self, wallet: &PeerId) -> Vec<Claim> {
        self.registry.find_by_wallet(wallet)
    }

    /// Publish a new claim. `verified` reflects whatever verification the
    /// caller (the wallet app) already ran out of band — see the `record`
    /// module doc.
    pub fn publish(
        &mut self,
        id: impl Into<String>,
        claim_type: ClaimType,
        value: impl Into<String>,
        verified: bool,
        supersedes: Option<ClaimId>,
        expires_at: Option<Timestamp>,
    ) -> Result<ClaimId, IdentityError> {
        let publish = ClaimPublish {
            id: ClaimId::new(id),
            wallet: self.gossip.node.local_peer_id(),
            wallet_public_key: self.gossip.public_key(),
            claim_type,
            value: value.into(),
            verified,
            supersedes,
            expires_at,
            timestamp: Timestamp::now(),
        };
        let bytes = wire::to_bytes(&publish).map_err(|_| IdentityError::MalformedClaim)?;
        let signed = SignedClaimPublish {
            signature: self.gossip.sign(&bytes),
            publish,
        };
        self.originate(protocol::EVENT_CREATED, &signed)?;
        Ok(signed.publish.id)
    }

    pub fn verify(&mut self, claim_id: ClaimId) -> Result<(), IdentityError> {
        let verify = ClaimVerify {
            claim_id,
            wallet: self.gossip.node.local_peer_id(),
            timestamp: Timestamp::now(),
        };
        let bytes = wire::to_bytes(&verify).map_err(|_| IdentityError::MalformedClaim)?;
        let signed = SignedClaimVerify {
            signature: self.gossip.sign(&bytes),
            verify,
        };
        self.originate(protocol::EVENT_VERIFIED, &signed)
    }

    pub fn revoke(&mut self, claim_id: ClaimId) -> Result<(), IdentityError> {
        let revoke = ClaimRevoke {
            claim_id,
            wallet: self.gossip.node.local_peer_id(),
            timestamp: Timestamp::now(),
        };
        let bytes = wire::to_bytes(&revoke).map_err(|_| IdentityError::MalformedClaim)?;
        let signed = SignedClaimRevoke {
            signature: self.gossip.sign(&bytes),
            revoke,
        };
        self.originate(protocol::EVENT_REVOKED, &signed)
    }

    fn originate(
        &mut self,
        event_type: &str,
        payload: &impl serde::Serialize,
    ) -> Result<(), IdentityError> {
        let bytes = wire::to_bytes(payload).map_err(|_| IdentityError::MalformedClaim)?;
        let event_type = EventType::new(event_type)
            .expect("identity event names are all valid PascalCase identifiers");
        self.gossip
            .originate(
                event_type,
                protocol::OFS_SPEC,
                Priority::Reputation,
                8,
                bytes,
            )
            .map(|_| ())
            .map_err(|_| IdentityError::Unauthorized)
    }

    pub async fn drive_once(&mut self) {
        self.gossip.drive_once().await;
    }
}
