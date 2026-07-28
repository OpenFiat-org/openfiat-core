//! Drives one node's session index: applies incoming gossip events
//! automatically and provides the operations that originate new ones.

use crate::error::SessionError;
use crate::events::{
    SessionCreate, SessionMigrate, SessionRenew, SessionRevoke, SignedSessionCreate,
    SignedSessionMigrate, SignedSessionRenew, SignedSessionRevoke,
};
use crate::protocol;
use crate::record::{Session, SessionId};
use crate::store::SessionRegistry;
use openfiat_gossip::GossipService;
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{EventType, PeerId, Priority, Timestamp};
use std::rc::Rc;
use std::time::Duration;

pub struct SessionService<S> {
    pub gossip: GossipService<S>,
    registry: Rc<SessionRegistry<S>>,
}

impl<S: KvStore + 'static> SessionService<S> {
    pub fn new(mut gossip: GossipService<S>, store: S) -> Self {
        let registry = Rc::new(SessionRegistry::new(store));
        let handler_registry = Rc::clone(&registry);
        gossip.add_event_handler(move |event| handler_registry.apply_event(event));
        Self { gossip, registry }
    }

    pub fn registry(&self) -> Rc<SessionRegistry<S>> {
        Rc::clone(&self.registry)
    }

    pub fn get(&self, id: &SessionId) -> Option<Session> {
        self.registry.get(id)
    }

    pub fn find_by_wallet(&self, wallet: &PeerId) -> Vec<Session> {
        self.registry.find_by_wallet(wallet)
    }

    /// §5: establishes a new session under this node's own wallet
    /// identity, hosted on this node, using the default lifetime unless
    /// `lifetime` is given.
    pub fn establish(
        &mut self,
        id: impl Into<String>,
        client: impl Into<String>,
        permissions: Vec<String>,
        lifetime: Option<Duration>,
    ) -> Result<SessionId, SessionError> {
        let now = Timestamp::now();
        let lifetime = lifetime.unwrap_or(protocol::DEFAULT_SESSION_LIFETIME);
        let create = SessionCreate {
            id: SessionId::new(id),
            wallet: self.gossip.node.local_peer_id(),
            wallet_public_key: self.gossip.public_key(),
            client: client.into(),
            host_node: self.gossip.node.local_peer_id(),
            permissions,
            timestamp: now,
            expires_at: Timestamp::from_millis(now.as_millis() + lifetime.as_millis() as u64),
        };
        let bytes = wire::to_bytes(&create).map_err(|_| SessionError::MalformedSession)?;
        let signed = SignedSessionCreate {
            signature: self.gossip.sign(&bytes),
            create,
        };
        self.originate(protocol::EVENT_ESTABLISHED, &signed)?;
        Ok(signed.create.id)
    }

    pub fn renew(
        &mut self,
        session_id: SessionId,
        version: u64,
        lifetime: Option<Duration>,
    ) -> Result<(), SessionError> {
        let lifetime = lifetime.unwrap_or(protocol::DEFAULT_SESSION_LIFETIME);
        let now = Timestamp::now();
        let renew = SessionRenew {
            session_id,
            wallet: self.gossip.node.local_peer_id(),
            new_expires_at: Timestamp::from_millis(now.as_millis() + lifetime.as_millis() as u64),
            version,
            timestamp: now,
        };
        let bytes = wire::to_bytes(&renew).map_err(|_| SessionError::MalformedSession)?;
        let signed = SignedSessionRenew {
            signature: self.gossip.sign(&bytes),
            renew,
        };
        self.originate(protocol::EVENT_RENEWED, &signed)
    }

    pub fn revoke(&mut self, session_id: SessionId) -> Result<(), SessionError> {
        let revoke = SessionRevoke {
            session_id,
            wallet: self.gossip.node.local_peer_id(),
            timestamp: Timestamp::now(),
        };
        let bytes = wire::to_bytes(&revoke).map_err(|_| SessionError::MalformedSession)?;
        let signed = SignedSessionRevoke {
            signature: self.gossip.sign(&bytes),
            revoke,
        };
        self.originate(protocol::EVENT_REVOKED, &signed)
    }

    pub fn migrate(
        &mut self,
        session_id: SessionId,
        new_host_node: PeerId,
        version: u64,
    ) -> Result<(), SessionError> {
        let migrate = SessionMigrate {
            session_id,
            wallet: self.gossip.node.local_peer_id(),
            new_host_node,
            version,
            timestamp: Timestamp::now(),
        };
        let bytes = wire::to_bytes(&migrate).map_err(|_| SessionError::MalformedSession)?;
        let signed = SignedSessionMigrate {
            signature: self.gossip.sign(&bytes),
            migrate,
        };
        self.originate(protocol::EVENT_MIGRATED, &signed)
    }

    fn originate(
        &mut self,
        event_type: &str,
        payload: &impl serde::Serialize,
    ) -> Result<(), SessionError> {
        let bytes = wire::to_bytes(payload).map_err(|_| SessionError::MalformedSession)?;
        let event_type = EventType::new(event_type)
            .expect("session event names are all valid PascalCase identifiers");
        self.gossip
            .originate(
                event_type,
                protocol::OFS_SPEC,
                Priority::SessionReservationSettlement,
                8,
                bytes,
            )
            .map(|_| ())
            .map_err(|_| SessionError::Unauthorized)
    }

    pub async fn drive_once(&mut self) {
        self.gossip.drive_once().await;
    }
}
