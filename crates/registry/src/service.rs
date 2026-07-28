//! Drives one node's registry: applies incoming gossip events to the
//! local registry automatically (via `GossipService`'s event hook) and
//! provides the register/update-health/withdraw operations that originate
//! new ones.

use crate::error::RegistryError;
use crate::health::{HealthState, HealthUpdate, SignedHealthUpdate};
use crate::protocol;
use crate::record::ServiceRecord;
use crate::registration::{Registration, SignedRegistration};
use crate::store::Registry;
use crate::withdrawal::{SignedWithdrawal, Withdrawal};
use openfiat_gossip::GossipService;
use openfiat_serialization::json;
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{EventType, Priority, ServiceId, ServiceType, Timestamp};
use std::rc::Rc;
use std::time::Duration;

pub struct RegistryService<S> {
    pub gossip: GossipService<S>,
    registry: Rc<Registry<S>>,
}

impl<S: KvStore + 'static> RegistryService<S> {
    pub fn new(mut gossip: GossipService<S>, store: S) -> Self {
        let registry = Rc::new(Registry::new(store));
        let handler_registry = Rc::clone(&registry);
        gossip.add_event_handler(move |event| handler_registry.apply_event(event));
        Self { gossip, registry }
    }

    pub fn get(&self, id: &ServiceId) -> Option<ServiceRecord> {
        self.registry.get(id)
    }

    pub fn all(&self) -> Vec<ServiceRecord> {
        self.registry.all()
    }

    pub fn find_by_type(&self, service_type: ServiceType) -> Vec<ServiceRecord> {
        self.registry.find_by_type(service_type)
    }

    /// §18: drop services this node hasn't seen a health update for
    /// within `threshold`. Purely local bookkeeping — every node runs
    /// this independently against its own replica.
    pub fn expire_stale(&self, threshold: Duration) -> usize {
        self.registry.expire_stale(threshold)
    }

    /// §7: register a new service under this node's own identity.
    #[allow(clippy::too_many_arguments)]
    pub fn register(
        &mut self,
        service_id: impl Into<String>,
        service_type: ServiceType,
        endpoints: Vec<String>,
        supported_ofs: Vec<u16>,
        region: Option<String>,
        capabilities: Vec<String>,
        pricing: Option<String>,
    ) -> Result<ServiceId, RegistryError> {
        let registration = Registration {
            service_id: ServiceId::new(service_id),
            service_type,
            provider: self.gossip.node.local_peer_id(),
            provider_public_key: self.gossip.public_key(),
            endpoints,
            supported_ofs,
            region,
            capabilities,
            pricing,
            timestamp: Timestamp::now(),
        };
        let signed = self.sign_registration(registration);
        // Local application happens via the event handler when
        // `originate` stores it — see `GossipService::notify`.
        self.originate(protocol::EVENT_REGISTERED, &signed)?;
        Ok(signed.registration.service_id)
    }

    fn sign_registration(&self, registration: Registration) -> SignedRegistration {
        let bytes = json::to_bytes(&registration).expect("Registration always serializes");
        SignedRegistration {
            signature: self.gossip.sign(&bytes),
            registration,
        }
    }

    /// §11: publish a health update for a service this node itself provides.
    pub fn publish_health(
        &mut self,
        service_id: ServiceId,
        state: HealthState,
    ) -> Result<(), RegistryError> {
        let update = HealthUpdate {
            service_id,
            provider: self.gossip.node.local_peer_id(),
            state,
            timestamp: Timestamp::now(),
        };
        let bytes = json::to_bytes(&update).map_err(|_| RegistryError::MalformedRegistration)?;
        let signed = SignedHealthUpdate {
            signature: self.gossip.sign(&bytes),
            update,
        };
        self.originate(protocol::EVENT_UPDATED, &signed)
    }

    /// §17: voluntarily withdraw a service this node itself provides.
    pub fn withdraw(&mut self, service_id: ServiceId) -> Result<(), RegistryError> {
        let withdrawal = Withdrawal {
            service_id,
            provider: self.gossip.node.local_peer_id(),
            timestamp: Timestamp::now(),
        };
        let bytes =
            json::to_bytes(&withdrawal).map_err(|_| RegistryError::MalformedRegistration)?;
        let signed = SignedWithdrawal {
            signature: self.gossip.sign(&bytes),
            withdrawal,
        };
        self.originate(protocol::EVENT_UNREGISTERED, &signed)
    }

    fn originate(
        &mut self,
        event_type: &str,
        payload: &impl serde::Serialize,
    ) -> Result<(), RegistryError> {
        let bytes = wire::to_bytes(payload).map_err(|_| RegistryError::MalformedRegistration)?;
        let event_type = EventType::new(event_type)
            .expect("registry event names are all valid PascalCase identifiers");
        self.gossip
            .originate(
                event_type,
                protocol::OFS_SPEC,
                Priority::BackgroundSync,
                8,
                bytes,
            )
            .map(|_| ())
            .map_err(|_| RegistryError::GossipRejected)
    }

    pub async fn drive_once(&mut self) {
        self.gossip.drive_once().await;
    }
}
