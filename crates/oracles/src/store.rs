//! The replicated local oracle index, sharing a handle to the node's
//! service registry (§5/§15: only a registered market-data provider may
//! publish).

use crate::error::OracleError;
use crate::events::SignedOraclePublish;
use crate::protocol;
use crate::record::{OracleCategory, OracleData, OracleId, OracleRecord};
use openfiat_registry::Registry;
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{EventEnvelope, ServiceType, Timestamp};
use std::rc::Rc;

const COLUMN_FAMILY: &str = "oracle_records";

pub struct OracleIndex<S> {
    store: S,
    services: Rc<Registry<S>>,
}

impl<S: KvStore> OracleIndex<S> {
    pub fn new(store: S, services: Rc<Registry<S>>) -> Self {
        Self { store, services }
    }

    pub fn get(&self, id: &OracleId) -> Option<OracleRecord> {
        let bytes = self.store.get(COLUMN_FAMILY, id.as_str().as_bytes()).ok().flatten()?;
        wire::from_bytes(&bytes).ok()
    }

    fn put(&self, record: &OracleRecord) {
        if let Ok(bytes) = wire::to_bytes(record) {
            let _ = self.store.put(COLUMN_FAMILY, record.id.as_str().as_bytes(), &bytes);
        }
    }

    pub fn all(&self) -> Vec<OracleRecord> {
        self.store.iter_prefix(COLUMN_FAMILY, &[]).unwrap_or_default().into_iter().filter_map(|(_, value)| wire::from_bytes(&value).ok()).collect()
    }

    pub fn find_by_category(&self, category: OracleCategory) -> Vec<OracleRecord> {
        self.all().into_iter().filter(|record| record.data.category() == category).collect()
    }

    /// §5/§8/§15: only a registered market-data provider may publish,
    /// and only a strictly newer version than whatever's already on file
    /// for this Oracle ID.
    pub fn apply_publish(&self, signed: SignedOraclePublish) -> Result<OracleId, OracleError> {
        signed.verify()?;
        let publish = signed.publish;
        if !self.services.all().into_iter().any(|service| service.provider == publish.provider && matches!(service.service_type, ServiceType::MarketData(_))) {
            return Err(OracleError::Unauthorized);
        }
        if publish.expires_at.as_millis() <= publish.timestamp.as_millis() {
            return Err(OracleError::AlreadyExpired);
        }
        if let Some(existing) = self.get(&publish.id)
            && publish.version <= existing.version
        {
            return Err(OracleError::StaleVersion);
        }

        self.put(&OracleRecord {
            id: publish.id.clone(),
            provider: publish.provider,
            provider_public_key: publish.provider_public_key,
            data: publish.data,
            version: publish.version,
            published_at: publish.timestamp,
            expires_at: publish.expires_at,
        });
        Ok(publish.id)
    }

    /// §11: the median exchange rate among every current (non-expired)
    /// `base`/`quote` record from any provider — "no single provider
    /// should become a mandatory dependency."
    pub fn median_exchange_rate(&self, base: &str, quote: &str, now: Timestamp) -> Option<f64> {
        let mut rates: Vec<f64> = self
            .all()
            .into_iter()
            .filter(|record| record.is_current(now))
            .filter_map(|record| match record.data {
                OracleData::ExchangeRate { base: b, quote: q, rate } if b == base && q == quote => Some(rate),
                _ => None,
            })
            .collect();
        if rates.is_empty() {
            return None;
        }
        rates.sort_by(|a, b| a.total_cmp(b));
        Some(rates[rates.len() / 2])
    }

    pub fn apply_event(&self, event: &EventEnvelope) {
        if event.ofs_spec != protocol::OFS_SPEC || event.event_type.as_str() != protocol::EVENT_PUBLISHED {
            return;
        }
        if let Ok(signed) = wire::from_bytes(&event.payload) {
            let _ = self.apply_publish(signed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::OraclePublish;
    use openfiat_crypto::Keypair;
    use openfiat_network::identity::peer_id_from_public_key;
    use openfiat_registry::{Registration, SignedRegistration};
    use openfiat_storage::mem::MemoryStore;
    use openfiat_types::{MarketDataService, ServiceId};

    fn registered_provider(seed: u8) -> (Keypair, Rc<Registry<MemoryStore>>) {
        let keypair = Keypair::from_seed([seed; 32]);
        let services = Rc::new(Registry::new(MemoryStore::new()));
        let registration = Registration {
            service_id: ServiceId::new(format!("oracle-svc-{seed}")),
            service_type: ServiceType::MarketData(MarketDataService::FxOracle),
            provider: peer_id_from_public_key(&keypair.public_key()).unwrap(),
            provider_public_key: keypair.public_key(),
            endpoints: vec![],
            supported_ofs: vec![7000],
            region: None,
            capabilities: vec!["USD/KES".to_string()],
            pricing: None,
            timestamp: Timestamp::now(),
        };
        services.apply_registration(SignedRegistration::sign(registration, &keypair)).unwrap();
        (keypair, services)
    }

    fn publish(provider: &Keypair, id: &str, version: u64, rate: f64) -> OraclePublish {
        OraclePublish {
            id: OracleId::new(id),
            provider: peer_id_from_public_key(&provider.public_key()).unwrap(),
            provider_public_key: provider.public_key(),
            data: OracleData::ExchangeRate { base: "USD".to_string(), quote: "KES".to_string(), rate },
            version,
            timestamp: Timestamp::now(),
            expires_at: Timestamp::from_millis(Timestamp::now().as_millis() + 60_000),
        }
    }

    #[test]
    fn an_unregistered_publisher_is_rejected() {
        let services = Rc::new(Registry::new(MemoryStore::new()));
        let registry = OracleIndex::new(MemoryStore::new(), services);
        let stranger = Keypair::generate();
        let result = registry.apply_publish(SignedOraclePublish::sign(publish(&stranger, "usd-kes", 1, 129.52), &stranger));
        assert_eq!(result, Err(OracleError::Unauthorized));
    }

    #[test]
    fn a_registered_provider_can_publish_and_it_is_queryable() {
        let (provider, services) = registered_provider(1);
        let registry = OracleIndex::new(MemoryStore::new(), services);
        let id = registry.apply_publish(SignedOraclePublish::sign(publish(&provider, "usd-kes", 1, 129.52), &provider)).unwrap();
        assert_eq!(registry.get(&id).unwrap().data, OracleData::ExchangeRate { base: "USD".to_string(), quote: "KES".to_string(), rate: 129.52 });
    }

    #[test]
    fn a_stale_version_is_rejected() {
        let (provider, services) = registered_provider(1);
        let registry = OracleIndex::new(MemoryStore::new(), services);
        registry.apply_publish(SignedOraclePublish::sign(publish(&provider, "usd-kes", 2, 129.52), &provider)).unwrap();
        let result = registry.apply_publish(SignedOraclePublish::sign(publish(&provider, "usd-kes", 2, 129.60), &provider));
        assert_eq!(result, Err(OracleError::StaleVersion));
    }

    #[test]
    fn an_already_expired_record_is_rejected() {
        let (provider, services) = registered_provider(1);
        let registry = OracleIndex::new(MemoryStore::new(), services);
        let mut bad = publish(&provider, "usd-kes", 1, 129.52);
        bad.expires_at = bad.timestamp;
        let result = registry.apply_publish(SignedOraclePublish::sign(bad, &provider));
        assert_eq!(result, Err(OracleError::AlreadyExpired));
    }

    #[test]
    fn the_median_rate_matches_the_spec_worked_example() {
        let services = Rc::new(Registry::new(MemoryStore::new()));
        let registry = OracleIndex::new(MemoryStore::new(), Rc::clone(&services));
        let rates = [129.50, 129.54, 129.51];
        for (i, rate) in rates.iter().enumerate() {
            let provider = Keypair::from_seed([(i + 1) as u8; 32]);
            let registration = Registration {
                service_id: ServiceId::new(format!("svc-{i}")),
                service_type: ServiceType::MarketData(MarketDataService::FxOracle),
                provider: peer_id_from_public_key(&provider.public_key()).unwrap(),
                provider_public_key: provider.public_key(),
                endpoints: vec![],
                supported_ofs: vec![7000],
                region: None,
                capabilities: vec![],
                pricing: None,
                timestamp: Timestamp::now(),
            };
            services.apply_registration(SignedRegistration::sign(registration, &provider)).unwrap();
            registry.apply_publish(SignedOraclePublish::sign(publish(&provider, &format!("usd-kes-{i}"), 1, *rate), &provider)).unwrap();
        }
        assert_eq!(registry.median_exchange_rate("USD", "KES", Timestamp::now()), Some(129.51));
    }

    #[test]
    fn an_expired_record_is_excluded_from_the_median() {
        let (provider, services) = registered_provider(1);
        let registry = OracleIndex::new(MemoryStore::new(), services);
        let mut record = publish(&provider, "usd-kes", 1, 129.52);
        record.expires_at = Timestamp::from_millis(record.timestamp.as_millis() + 1);
        registry.apply_publish(SignedOraclePublish::sign(record, &provider)).unwrap();

        let far_future = Timestamp::from_millis(Timestamp::now().as_millis() + 60_000);
        assert_eq!(registry.median_exchange_rate("USD", "KES", far_future), None);
    }
}
