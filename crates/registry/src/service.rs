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
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{EventType, Priority, ServiceId, ServiceType, Timestamp};
use std::rc::Rc;
use std::time::Duration;

pub struct RegistryService<S> {
    pub gossip: GossipService<S>,
    registry: Rc<Registry<S>>,
}

/// The half of a registration a caller supplies (§7), as one value
/// rather than as a positional argument list.
///
/// The other half — provider peer id, public key, timestamp — is the
/// node's own and is filled in by [`RegistryService::register`], which
/// is the point: a caller cannot register a service under someone
/// else's identity by passing the wrong argument.
///
/// A struct rather than parameters because the list had reached eight
/// and needed `#[allow(clippy::too_many_arguments)]` to compile, and
/// four of the eight were `Option`s of compatible types — `region` and a
/// branding field could be swapped at a call site with nothing to catch
/// it. Branding would have made it nine.
#[derive(Debug, Clone)]
pub struct ServiceListing {
    pub service_id: String,
    pub service_type: ServiceType,
    pub endpoints: Vec<String>,
    pub supported_ofs: Vec<u16>,
    /// Self-declared. Nothing observes it — see
    /// `Registration::region`.
    pub region: Option<String>,
    pub capabilities: Vec<String>,
    /// Name, description, logo and website. Self-asserted; bounded by
    /// [`crate::ServiceBranding::validate`].
    pub branding: Option<crate::branding::ServiceBranding>,
    pub pricing: Option<crate::pricing::ServicePricing>,
    pub payout_wallet: Option<String>,
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
    pub fn register(&mut self, listing: ServiceListing) -> Result<ServiceId, RegistryError> {
        let registration = Registration {
            service_id: ServiceId::new(listing.service_id),
            service_type: listing.service_type,
            provider: self.gossip.node.local_peer_id(),
            provider_public_key: self.gossip.public_key(),
            endpoints: listing.endpoints,
            supported_ofs: listing.supported_ofs,
            region: listing.region,
            capabilities: listing.capabilities,
            branding: listing.branding,
            pricing: listing.pricing,
            payout_wallet: listing.payout_wallet,
            timestamp: Timestamp::now(),
        };
        let signed = self.sign_registration(registration);
        // Local application happens via the event handler when
        // `originate` stores it — see `GossipService::notify`.
        self.originate(protocol::EVENT_REGISTERED, &signed)?;
        Ok(signed.registration.service_id)
    }

    fn sign_registration(&self, registration: Registration) -> SignedRegistration {
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::REGISTRATION,
            &registration,
        )
        .expect("Registration always serializes");
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
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::HEALTH_UPDATE,
            &update,
        )
        .map_err(|_| RegistryError::MalformedRegistration)?;
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
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::WITHDRAWAL,
            &withdrawal,
        )
        .map_err(|_| RegistryError::MalformedRegistration)?;
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
