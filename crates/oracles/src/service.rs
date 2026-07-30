//! Drives one node's oracle index: applies incoming gossip events
//! automatically and provides the operation that originates new ones.
//! Provider registration reuses `openfiat-registry` directly — this
//! crate has no registration event of its own.

use crate::error::OracleError;
use crate::events::{OraclePublish, SignedOraclePublish};
use crate::protocol;
use crate::record::{OracleCategory, OracleData, OracleId, OracleRecord};
use crate::store::OracleIndex;
use openfiat_gossip::GossipService;
use openfiat_registry::Registry;
use openfiat_serialization::json;
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{EventType, Priority, Timestamp};
use std::rc::Rc;

pub struct OracleService<S> {
    pub gossip: GossipService<S>,
    registry: Rc<OracleIndex<S>>,
}

impl<S: KvStore + 'static> OracleService<S> {
    /// `services` is the shared handle from `RegistryService::registry`
    /// on the same node — see `OracleIndex`.
    pub fn new(mut gossip: GossipService<S>, store: S, services: Rc<Registry<S>>) -> Self {
        let registry = Rc::new(OracleIndex::new(store, services));
        let handler_registry = Rc::clone(&registry);
        gossip.add_event_handler(move |event| handler_registry.apply_event(event));
        Self { gossip, registry }
    }

    pub fn get(&self, id: &OracleId) -> Option<OracleRecord> {
        self.registry.get(id)
    }

    pub fn all(&self) -> Vec<OracleRecord> {
        self.registry.all()
    }

    pub fn find_by_category(&self, category: OracleCategory) -> Vec<OracleRecord> {
        self.registry.find_by_category(category)
    }

    pub fn median_exchange_rate(&self, base: &str, quote: &str) -> Option<f64> {
        self.registry
            .median_exchange_rate(base, quote, Timestamp::now())
    }

    /// [`OracleIndex::exchange_rate`] against this node's clock — what
    /// anything pricing a trade should read, rather than the `Option`
    /// above, so a lapsed feed is distinguishable from an unknown pair.
    pub fn exchange_rate(&self, base: &str, quote: &str) -> crate::store::ExchangeRateLookup {
        self.registry.exchange_rate(base, quote, Timestamp::now())
    }

    /// §8: publish a new or updated record under this node's own
    /// identity, `version` strictly greater than whatever's already on
    /// file (see `OracleIndex::apply_publish`).
    pub fn publish(
        &mut self,
        id: impl Into<String>,
        data: OracleData,
        version: u64,
        ttl: std::time::Duration,
    ) -> Result<OracleId, OracleError> {
        let now = Timestamp::now();
        let publish = OraclePublish {
            id: OracleId::new(id),
            provider: self.gossip.node.local_peer_id(),
            provider_public_key: self.gossip.public_key(),
            data,
            version,
            timestamp: now,
            expires_at: Timestamp::from_millis(now.as_millis() + ttl.as_millis() as u64),
        };
        let bytes = json::to_bytes(&publish).map_err(|_| OracleError::MalformedRecord)?;
        let signed = SignedOraclePublish {
            signature: self.gossip.sign(&bytes),
            publish,
        };
        self.originate(&signed)?;
        Ok(signed.publish.id)
    }

    fn originate(&mut self, payload: &impl serde::Serialize) -> Result<(), OracleError> {
        let bytes = wire::to_bytes(payload).map_err(|_| OracleError::MalformedRecord)?;
        let event_type = EventType::new(protocol::EVENT_PUBLISHED)
            .expect("oracle event name is a valid PascalCase identifier");
        self.gossip
            .originate(
                event_type,
                protocol::OFS_SPEC,
                Priority::Reputation,
                8,
                bytes,
            )
            .map(|_| ())
            .map_err(|_| OracleError::Unauthorized)
    }

    pub async fn drive_once(&mut self) {
        self.gossip.drive_once().await;
    }
}
