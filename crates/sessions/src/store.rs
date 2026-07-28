//! The replicated local session index (§10: "Sessions SHALL be
//! replicated across multiple peers... No single node owns a session").

use crate::error::SessionError;
use crate::events::{
    SignedSessionCreate, SignedSessionMigrate, SignedSessionRenew, SignedSessionRevoke,
};
use crate::protocol;
use crate::record::{Session, SessionId};
use openfiat_crypto::verify;
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{EventEnvelope, PeerId};

const COLUMN_FAMILY: &str = "sessions";

pub struct SessionRegistry<S> {
    store: S,
}

impl<S: KvStore> SessionRegistry<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }

    pub fn get(&self, id: &SessionId) -> Option<Session> {
        let bytes = self
            .store
            .get(COLUMN_FAMILY, id.as_str().as_bytes())
            .ok()
            .flatten()?;
        wire::from_bytes(&bytes).ok()
    }

    fn put(&self, session: &Session) {
        if let Ok(bytes) = wire::to_bytes(session) {
            let _ = self
                .store
                .put(COLUMN_FAMILY, session.id.as_str().as_bytes(), &bytes);
        }
    }

    pub fn all(&self) -> Vec<Session> {
        self.store
            .iter_prefix(COLUMN_FAMILY, &[])
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(_, value)| wire::from_bytes(&value).ok())
            .collect()
    }

    /// §20: every session for a wallet, including its own concurrent
    /// devices/clients.
    pub fn find_by_wallet(&self, wallet: &PeerId) -> Vec<Session> {
        self.all()
            .into_iter()
            .filter(|session| &session.wallet == wallet)
            .collect()
    }

    pub fn apply_create(&self, signed: SignedSessionCreate) -> Result<SessionId, SessionError> {
        signed.verify()?;
        let create = signed.create;
        if self.get(&create.id).is_some() {
            return Err(SessionError::DuplicateSessionId);
        }

        self.put(&Session {
            id: create.id.clone(),
            wallet: create.wallet,
            wallet_public_key: create.wallet_public_key,
            authenticated_at: create.timestamp,
            expires_at: create.expires_at,
            client: create.client,
            host_node: create.host_node,
            permissions: create.permissions,
            version: 0,
            revoked: false,
        });
        Ok(create.id)
    }

    /// §15/§18: extends `expires_at`; rejected if already revoked, or if
    /// `version` doesn't move the session strictly forward.
    pub fn apply_renew(&self, signed: SignedSessionRenew) -> Result<(), SessionError> {
        let mut session = self
            .get(&signed.renew.session_id)
            .ok_or(SessionError::SessionNotFound)?;
        if session.wallet != signed.renew.wallet {
            return Err(SessionError::Unauthorized);
        }
        let bytes = wire::to_bytes(&signed.renew).map_err(|_| SessionError::MalformedSession)?;
        verify(&session.wallet_public_key, &bytes, &signed.signature)
            .map_err(|_| SessionError::InvalidSignature)?;
        if session.revoked {
            return Err(SessionError::AlreadyRevoked);
        }
        if signed.renew.version <= session.version {
            return Err(SessionError::StaleVersion);
        }

        session.expires_at = signed.renew.new_expires_at;
        session.version = signed.renew.version;
        self.put(&session);
        Ok(())
    }

    /// §16: permanent — a revoked session stays revoked.
    pub fn apply_revoke(&self, signed: SignedSessionRevoke) -> Result<(), SessionError> {
        let mut session = self
            .get(&signed.revoke.session_id)
            .ok_or(SessionError::SessionNotFound)?;
        if session.wallet != signed.revoke.wallet {
            return Err(SessionError::Unauthorized);
        }
        let bytes = wire::to_bytes(&signed.revoke).map_err(|_| SessionError::MalformedSession)?;
        verify(&session.wallet_public_key, &bytes, &signed.signature)
            .map_err(|_| SessionError::InvalidSignature)?;
        if session.revoked {
            return Err(SessionError::AlreadyRevoked);
        }

        session.revoked = true;
        self.put(&session);
        Ok(())
    }

    /// §12: seamless migration to a new Primary Session Host.
    pub fn apply_migrate(&self, signed: SignedSessionMigrate) -> Result<(), SessionError> {
        let mut session = self
            .get(&signed.migrate.session_id)
            .ok_or(SessionError::SessionNotFound)?;
        if session.wallet != signed.migrate.wallet {
            return Err(SessionError::Unauthorized);
        }
        let bytes = wire::to_bytes(&signed.migrate).map_err(|_| SessionError::MalformedSession)?;
        verify(&session.wallet_public_key, &bytes, &signed.signature)
            .map_err(|_| SessionError::InvalidSignature)?;
        if session.revoked {
            return Err(SessionError::AlreadyRevoked);
        }
        if signed.migrate.version <= session.version {
            return Err(SessionError::StaleVersion);
        }

        session.host_node = signed.migrate.new_host_node;
        session.version = signed.migrate.version;
        self.put(&session);
        Ok(())
    }

    pub fn apply_event(&self, event: &EventEnvelope) {
        if event.ofs_spec != protocol::OFS_SPEC {
            return;
        }
        match event.event_type.as_str() {
            protocol::EVENT_ESTABLISHED => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_create(signed);
                }
            }
            protocol::EVENT_RENEWED => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_renew(signed);
                }
            }
            protocol::EVENT_REVOKED => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_revoke(signed);
                }
            }
            protocol::EVENT_MIGRATED => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_migrate(signed);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{SessionMigrate, SessionRenew, SessionRevoke};
    use openfiat_crypto::Keypair;
    use openfiat_network::identity::peer_id_from_public_key;
    use openfiat_storage::mem::MemoryStore;
    use openfiat_types::Timestamp;

    fn create_event(wallet: &Keypair, id: &str, host: &PeerId) -> crate::events::SessionCreate {
        crate::events::SessionCreate {
            id: SessionId::new(id),
            wallet: peer_id_from_public_key(&wallet.public_key()).unwrap(),
            wallet_public_key: wallet.public_key(),
            client: "web".to_string(),
            host_node: host.clone(),
            permissions: vec!["trade".to_string()],
            timestamp: Timestamp::now(),
            expires_at: Timestamp::from_millis(Timestamp::now().as_millis() + 3_600_000),
        }
    }

    #[test]
    fn a_session_is_queryable_after_creation() {
        let registry = SessionRegistry::new(MemoryStore::new());
        let wallet = Keypair::generate();
        let host = peer_id_from_public_key(&Keypair::generate().public_key()).unwrap();
        let id = registry
            .apply_create(SignedSessionCreate::sign(
                create_event(&wallet, "sess-1", &host),
                &wallet,
            ))
            .unwrap();
        assert!(registry.get(&id).unwrap().is_current(Timestamp::now()));
    }

    #[test]
    fn duplicate_session_ids_are_rejected() {
        let registry = SessionRegistry::new(MemoryStore::new());
        let wallet = Keypair::generate();
        let host = peer_id_from_public_key(&Keypair::generate().public_key()).unwrap();
        registry
            .apply_create(SignedSessionCreate::sign(
                create_event(&wallet, "sess-1", &host),
                &wallet,
            ))
            .unwrap();
        let result = registry.apply_create(SignedSessionCreate::sign(
            create_event(&wallet, "sess-1", &host),
            &wallet,
        ));
        assert_eq!(result, Err(SessionError::DuplicateSessionId));
    }

    #[test]
    fn a_different_wallet_cannot_renew_someone_elses_session() {
        let registry = SessionRegistry::new(MemoryStore::new());
        let wallet = Keypair::generate();
        let attacker = Keypair::generate();
        let host = peer_id_from_public_key(&Keypair::generate().public_key()).unwrap();
        let id = registry
            .apply_create(SignedSessionCreate::sign(
                create_event(&wallet, "sess-1", &host),
                &wallet,
            ))
            .unwrap();

        let renew = SessionRenew {
            session_id: id,
            wallet: peer_id_from_public_key(&wallet.public_key()).unwrap(),
            new_expires_at: Timestamp::now(),
            version: 1,
            timestamp: Timestamp::now(),
        };
        let result = registry.apply_renew(SignedSessionRenew::sign(renew, &attacker));
        assert_eq!(result, Err(SessionError::InvalidSignature));
    }

    #[test]
    fn renewal_extends_expiry_and_bumps_version() {
        let registry = SessionRegistry::new(MemoryStore::new());
        let wallet = Keypair::generate();
        let host = peer_id_from_public_key(&Keypair::generate().public_key()).unwrap();
        let id = registry
            .apply_create(SignedSessionCreate::sign(
                create_event(&wallet, "sess-1", &host),
                &wallet,
            ))
            .unwrap();

        let new_expiry =
            Timestamp::from_millis(registry.get(&id).unwrap().expires_at.as_millis() + 3_600_000);
        let renew = SessionRenew {
            session_id: id.clone(),
            wallet: peer_id_from_public_key(&wallet.public_key()).unwrap(),
            new_expires_at: new_expiry,
            version: 1,
            timestamp: Timestamp::now(),
        };
        registry
            .apply_renew(SignedSessionRenew::sign(renew, &wallet))
            .unwrap();

        let session = registry.get(&id).unwrap();
        assert_eq!(session.expires_at, new_expiry);
        assert_eq!(session.version, 1);
    }

    #[test]
    fn revocation_is_permanent_and_does_not_affect_other_sessions() {
        let registry = SessionRegistry::new(MemoryStore::new());
        let wallet = Keypair::generate();
        let host = peer_id_from_public_key(&Keypair::generate().public_key()).unwrap();
        let id1 = registry
            .apply_create(SignedSessionCreate::sign(
                create_event(&wallet, "sess-1", &host),
                &wallet,
            ))
            .unwrap();
        let id2 = registry
            .apply_create(SignedSessionCreate::sign(
                create_event(&wallet, "sess-2", &host),
                &wallet,
            ))
            .unwrap();

        let revoke = SessionRevoke {
            session_id: id1.clone(),
            wallet: peer_id_from_public_key(&wallet.public_key()).unwrap(),
            timestamp: Timestamp::now(),
        };
        registry
            .apply_revoke(SignedSessionRevoke::sign(revoke, &wallet))
            .unwrap();

        assert!(!registry.get(&id1).unwrap().is_current(Timestamp::now()));
        assert!(registry.get(&id2).unwrap().is_current(Timestamp::now()));
        assert_eq!(
            registry
                .find_by_wallet(&peer_id_from_public_key(&wallet.public_key()).unwrap())
                .len(),
            2
        );

        let revoke_again = SessionRevoke {
            session_id: id1,
            wallet: peer_id_from_public_key(&wallet.public_key()).unwrap(),
            timestamp: Timestamp::now(),
        };
        let result = registry.apply_revoke(SignedSessionRevoke::sign(revoke_again, &wallet));
        assert_eq!(result, Err(SessionError::AlreadyRevoked));
    }

    #[test]
    fn migration_moves_the_session_to_a_new_host_without_reauthentication() {
        let registry = SessionRegistry::new(MemoryStore::new());
        let wallet = Keypair::generate();
        let host_a = peer_id_from_public_key(&Keypair::generate().public_key()).unwrap();
        let host_b = peer_id_from_public_key(&Keypair::generate().public_key()).unwrap();
        let id = registry
            .apply_create(SignedSessionCreate::sign(
                create_event(&wallet, "sess-1", &host_a),
                &wallet,
            ))
            .unwrap();

        let migrate = SessionMigrate {
            session_id: id.clone(),
            wallet: peer_id_from_public_key(&wallet.public_key()).unwrap(),
            new_host_node: host_b.clone(),
            version: 1,
            timestamp: Timestamp::now(),
        };
        registry
            .apply_migrate(SignedSessionMigrate::sign(migrate, &wallet))
            .unwrap();

        let session = registry.get(&id).unwrap();
        assert_eq!(session.host_node, host_b);
        assert!(session.is_current(Timestamp::now()));
    }
}
