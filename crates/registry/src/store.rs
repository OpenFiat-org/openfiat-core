//! The replicated local registry (OFS-1500 §19, §23): "Every node
//! maintains a local copy... the reference implementation stores registry
//! entries in RocksDB." Generic over `KvStore`, same pattern as every
//! other store in this workspace.

use crate::error::RegistryError;
use crate::health::SignedHealthUpdate;
use crate::record::ServiceRecord;
use crate::registration::SignedRegistration;
use crate::withdrawal::SignedWithdrawal;
use crate::{parse_event, protocol};
use openfiat_crypto::verify;
use openfiat_serialization::json;
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{EventEnvelope, ServiceId, ServiceType, Timestamp};
use std::time::Duration;

const COLUMN_FAMILY: &str = "registry_services";

pub struct Registry<S> {
    store: S,
}

impl<S: KvStore> Registry<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }

    pub fn get(&self, id: &ServiceId) -> Option<ServiceRecord> {
        let bytes = self
            .store
            .get(COLUMN_FAMILY, id.as_str().as_bytes())
            .ok()
            .flatten()?;
        wire::from_bytes(&bytes).ok()
    }

    fn put(&self, record: &ServiceRecord) {
        if let Ok(bytes) = wire::to_bytes(record) {
            let _ = self
                .store
                .put(COLUMN_FAMILY, record.service_id.as_str().as_bytes(), &bytes);
        }
    }

    pub fn all(&self) -> Vec<ServiceRecord> {
        self.store
            .iter_prefix(COLUMN_FAMILY, &[])
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(_, value)| wire::from_bytes(&value).ok())
            .collect()
    }

    pub fn find_by_type(&self, service_type: ServiceType) -> Vec<ServiceRecord> {
        self.all()
            .into_iter()
            .filter(|record| record.service_type == service_type)
            .collect()
    }

    /// §7-8, §16, §21: accept a registration or an update to one, keyed by
    /// Service ID. An update is only accepted from the same provider that
    /// originally registered the ID.
    pub fn apply_registration(
        &self,
        signed: SignedRegistration,
    ) -> Result<ServiceId, RegistryError> {
        signed.verify()?;
        let id = signed.registration.service_id.clone();
        if let Some(existing) = self.get(&id)
            && existing.provider != signed.registration.provider
        {
            return Err(RegistryError::DuplicateServiceId);
        }
        self.put(&signed.into_record());
        Ok(id)
    }

    /// §11, §21: only accepted if the signer matches the service's
    /// on-file provider identity.
    pub fn apply_health_update(&self, signed: SignedHealthUpdate) -> Result<(), RegistryError> {
        let mut record = self
            .get(&signed.update.service_id)
            .ok_or(RegistryError::ServiceNotFound)?;
        if record.provider != signed.update.provider {
            return Err(RegistryError::UnauthorizedUpdate);
        }
        let bytes =
            json::to_bytes(&signed.update).map_err(|_| RegistryError::MalformedRegistration)?;
        verify(&record.provider_public_key, &bytes, &signed.signature)
            .map_err(|_| RegistryError::InvalidSignature)?;

        record.health = signed.update.state;
        record.last_health_update = signed.update.timestamp;
        self.put(&record);
        Ok(())
    }

    /// §17, §21: only accepted if the signer matches the service's
    /// on-file provider identity.
    pub fn apply_withdrawal(&self, signed: SignedWithdrawal) -> Result<(), RegistryError> {
        let record = self
            .get(&signed.withdrawal.service_id)
            .ok_or(RegistryError::ServiceNotFound)?;
        if record.provider != signed.withdrawal.provider {
            return Err(RegistryError::UnauthorizedUpdate);
        }
        let bytes =
            json::to_bytes(&signed.withdrawal).map_err(|_| RegistryError::MalformedRegistration)?;
        verify(&record.provider_public_key, &bytes, &signed.signature)
            .map_err(|_| RegistryError::InvalidSignature)?;

        let _ = self.store.delete(
            COLUMN_FAMILY,
            signed.withdrawal.service_id.as_str().as_bytes(),
        );
        Ok(())
    }

    /// §18: drop services that haven't published a health update within
    /// `threshold`. Returns how many were expired.
    pub fn expire_stale(&self, threshold: Duration) -> usize {
        let cutoff = Timestamp::now()
            .as_millis()
            .saturating_sub(threshold.as_millis() as u64);
        let stale: Vec<ServiceId> = self
            .all()
            .into_iter()
            .filter(|record| record.last_health_update.as_millis() < cutoff)
            .map(|record| record.service_id)
            .collect();
        let count = stale.len();
        for id in stale {
            let _ = self.store.delete(COLUMN_FAMILY, id.as_str().as_bytes());
        }
        count
    }

    /// Apply a gossip event to this registry, if it's one of ours
    /// (§19: replication happens purely by consuming gossip events).
    pub fn apply_event(&self, event: &EventEnvelope) {
        if event.ofs_spec != protocol::OFS_SPEC {
            return;
        }
        match parse_event(event) {
            Some(crate::RegistryEvent::Registered(signed)) => {
                let _ = self.apply_registration(signed);
            }
            Some(crate::RegistryEvent::Updated(signed)) => {
                let _ = self.apply_health_update(signed);
            }
            Some(crate::RegistryEvent::Unregistered(signed)) => {
                let _ = self.apply_withdrawal(signed);
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::{HealthState, HealthUpdate};
    use crate::registration::Registration;
    use crate::withdrawal::Withdrawal;
    use openfiat_crypto::Keypair;
    use openfiat_network::identity::peer_id_from_public_key;
    use openfiat_storage::mem::MemoryStore;
    use openfiat_types::{InfrastructureService, ServiceType};

    fn registration(keypair: &Keypair, id: &str) -> Registration {
        let provider = peer_id_from_public_key(&keypair.public_key()).unwrap();
        Registration {
            service_id: ServiceId::new(id),
            service_type: ServiceType::Infrastructure(InfrastructureService::SnapshotProvider),
            provider,
            provider_public_key: keypair.public_key(),
            endpoints: vec!["/ip4/127.0.0.1/udp/4001/quic-v1".to_string()],
            supported_ofs: vec![1300],
            region: None,
            capabilities: vec![],
            pricing: None,
            timestamp: Timestamp::now(),
        }
    }

    #[test]
    fn a_registration_is_queryable_after_being_applied() {
        let registry = Registry::new(MemoryStore::new());
        let keypair = Keypair::generate();
        let signed = SignedRegistration::sign(registration(&keypair, "svc-1"), &keypair);
        let id = registry.apply_registration(signed).unwrap();
        assert!(registry.get(&id).is_some());
    }

    #[test]
    fn an_update_from_a_different_provider_is_rejected() {
        let registry = Registry::new(MemoryStore::new());
        let owner = Keypair::generate();
        registry
            .apply_registration(SignedRegistration::sign(
                registration(&owner, "svc-1"),
                &owner,
            ))
            .unwrap();

        let attacker = Keypair::generate();
        let mut impostor = registration(&owner, "svc-1");
        impostor.provider_public_key = attacker.public_key();
        impostor.provider = peer_id_from_public_key(&attacker.public_key()).unwrap();
        let result = registry.apply_registration(SignedRegistration::sign(impostor, &attacker));
        assert_eq!(result, Err(RegistryError::DuplicateServiceId));
    }

    #[test]
    fn health_update_from_the_owner_is_accepted() {
        let registry = Registry::new(MemoryStore::new());
        let owner = Keypair::generate();
        let id = registry
            .apply_registration(SignedRegistration::sign(
                registration(&owner, "svc-1"),
                &owner,
            ))
            .unwrap();

        let update = HealthUpdate {
            service_id: id.clone(),
            provider: peer_id_from_public_key(&owner.public_key()).unwrap(),
            state: HealthState::Degraded,
            timestamp: Timestamp::now(),
        };
        registry
            .apply_health_update(SignedHealthUpdate::sign(update, &owner))
            .unwrap();
        assert_eq!(registry.get(&id).unwrap().health, HealthState::Degraded);
    }

    #[test]
    fn health_update_from_an_impostor_is_rejected() {
        let registry = Registry::new(MemoryStore::new());
        let owner = Keypair::generate();
        let id = registry
            .apply_registration(SignedRegistration::sign(
                registration(&owner, "svc-1"),
                &owner,
            ))
            .unwrap();

        let attacker = Keypair::generate();
        let update = HealthUpdate {
            service_id: id,
            provider: peer_id_from_public_key(&owner.public_key()).unwrap(),
            state: HealthState::Offline,
            timestamp: Timestamp::now(),
        };
        let result = registry.apply_health_update(SignedHealthUpdate::sign(update, &attacker));
        assert_eq!(result, Err(RegistryError::InvalidSignature));
    }

    #[test]
    fn withdrawal_removes_the_service() {
        let registry = Registry::new(MemoryStore::new());
        let owner = Keypair::generate();
        let id = registry
            .apply_registration(SignedRegistration::sign(
                registration(&owner, "svc-1"),
                &owner,
            ))
            .unwrap();

        let withdrawal = Withdrawal {
            service_id: id.clone(),
            provider: peer_id_from_public_key(&owner.public_key()).unwrap(),
            timestamp: Timestamp::now(),
        };
        registry
            .apply_withdrawal(SignedWithdrawal::sign(withdrawal, &owner))
            .unwrap();
        assert!(registry.get(&id).is_none());
    }

    #[test]
    fn expire_stale_removes_services_past_the_threshold() {
        let registry = Registry::new(MemoryStore::new());
        let owner = Keypair::generate();
        let mut reg = registration(&owner, "svc-1");
        reg.timestamp = Timestamp::from_millis(0);
        let id = registry
            .apply_registration(SignedRegistration::sign(reg, &owner))
            .unwrap();

        let removed = registry.expire_stale(Duration::from_secs(60));
        assert_eq!(removed, 1);
        assert!(registry.get(&id).is_none());
    }
}
