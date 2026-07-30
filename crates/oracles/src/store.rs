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

/// How long an already-expired record is kept before deletion.
///
/// Expiry and deletion are separate on purpose. A record past
/// `expires_at` is already refused by every reader — it is invalid, not
/// absent — and that distinction is worth something: "this rate expired
/// two hours ago" is a better answer than silence, and it is how an
/// operator sees a feed has died rather than that a provider never
/// existed. #140 was diagnosed exactly that way.
///
/// So a record is kept for a grace period past expiry, long enough to
/// explain itself, and only then deleted. `[PROPOSED — NEEDS SIGN-OFF]`
/// 7 days.
pub const EXPIRED_GRACE: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 60 * 60);

/// The result of asking this index for a pair's §11 median rate.
///
/// Three outcomes rather than `Option<f64>` because the two failures are
/// operationally different — see [`OracleIndex::exchange_rate`] — and
/// because collapsing them is how a caller ends up treating "the feed
/// died" as "this pair isn't supported" and quietly moving on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExchangeRateLookup {
    /// A median assembled from at least one unexpired record, good until
    /// `expires_at` (the earliest expiry among its contributors).
    Current { rate: f64, expires_at: Timestamp },
    /// The pair is published, but every record for it has expired. §12:
    /// "Expired data SHOULD NOT be treated as current" — so this is not a
    /// rate, deliberately, however recently it lapsed.
    Stale,
    /// No provider publishes this pair at all.
    NoData,
}

pub struct OracleIndex<S> {
    store: S,
    services: Rc<Registry<S>>,
}

impl<S: KvStore> OracleIndex<S> {
    pub fn new(store: S, services: Rc<Registry<S>>) -> Self {
        Self { store, services }
    }

    pub fn get(&self, id: &OracleId) -> Option<OracleRecord> {
        let bytes = self
            .store
            .get(COLUMN_FAMILY, id.as_str().as_bytes())
            .ok()
            .flatten()?;
        wire::from_bytes(&bytes).ok()
    }

    fn put(&self, record: &OracleRecord) {
        if let Ok(bytes) = wire::to_bytes(record) {
            let _ = self
                .store
                .put(COLUMN_FAMILY, record.id.as_str().as_bytes(), &bytes);
        }
    }

    /// Deletes records that expired longer ago than [`EXPIRED_GRACE`].
    ///
    /// Nothing aggregates the history of this family — no counter, no
    /// reputation figure, no earnings total is derived by scanning it —
    /// so dropping an old record changes no answer except that record's
    /// own. That is what makes this safe to prune where the marketplace
    /// records are not.
    pub fn prune_expired(&self, now: openfiat_types::Timestamp) -> usize {
        let cutoff = now
            .as_millis()
            .saturating_sub(EXPIRED_GRACE.as_millis() as u64);
        let mut dropped = 0;
        for (key, value) in self
            .store
            .iter_prefix(COLUMN_FAMILY, &[])
            .unwrap_or_default()
        {
            let Ok(record) = wire::from_bytes::<OracleRecord>(&value) else {
                continue;
            };
            let expired_at = Some(record.expires_at);
            if let Some(at) = expired_at
                && at.as_millis() < cutoff
                && self.store.delete(COLUMN_FAMILY, &key).is_ok()
            {
                dropped += 1;
            }
        }
        dropped
    }

    pub fn all(&self) -> Vec<OracleRecord> {
        self.store
            .iter_prefix(COLUMN_FAMILY, &[])
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(_, value)| wire::from_bytes(&value).ok())
            .collect()
    }

    pub fn find_by_category(&self, category: OracleCategory) -> Vec<OracleRecord> {
        self.all()
            .into_iter()
            .filter(|record| record.data.category() == category)
            .collect()
    }

    /// §5/§8/§15: only a registered market-data provider may publish,
    /// and only a strictly newer version than whatever's already on file
    /// for this Oracle ID.
    pub fn apply_publish(&self, signed: SignedOraclePublish) -> Result<OracleId, OracleError> {
        signed.verify()?;
        let publish = signed.publish;
        if !self.services.all().into_iter().any(|service| {
            service.provider == publish.provider
                && matches!(service.service_type, ServiceType::MarketData(_))
        }) {
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
        match self.exchange_rate(base, quote, now) {
            ExchangeRateLookup::Current { rate, .. } => Some(rate),
            _ => None,
        }
    }

    /// The same §11 median as [`Self::median_exchange_rate`], but saying
    /// *why* there is no rate when there isn't one, and until when the
    /// answer holds when there is.
    ///
    /// Anything that prices a trade off this needs both. "Nobody publishes
    /// USDC/KES" and "every provider who does has gone stale" are the same
    /// `None` to a caller, but they are a missing integration and a broken
    /// feed respectively — and a caller that pins a number (a reservation)
    /// has to know how long the number it pinned was actually good for.
    pub fn exchange_rate(&self, base: &str, quote: &str, now: Timestamp) -> ExchangeRateLookup {
        // Every matching record at any freshness: telling "absent" from
        // "expired" apart means looking at the expired ones too, so the
        // expiry filter is applied below rather than here.
        let matching: Vec<(f64, Timestamp)> = self
            .all()
            .into_iter()
            .filter_map(|record| match record.data {
                OracleData::ExchangeRate {
                    base: ref b,
                    quote: ref q,
                    rate,
                } if b == base && q == quote => Some((rate, record.expires_at)),
                _ => None,
            })
            .collect();
        if matching.is_empty() {
            return ExchangeRateLookup::NoData;
        }

        let mut current: Vec<(f64, Timestamp)> = matching
            .into_iter()
            .filter(|(_, expires_at)| now.as_millis() < expires_at.as_millis())
            .collect();
        if current.is_empty() {
            return ExchangeRateLookup::Stale;
        }

        // The median is only stable until the first contributor lapses, so
        // that — not the latest expiry — is how long this answer is good
        // for. Taking the latest would keep quoting a median assembled from
        // records that have already gone stale.
        let expires_at = current
            .iter()
            .map(|(_, expires_at)| *expires_at)
            .min()
            .expect("`current` is non-empty");

        current.sort_by(|(a, _), (b, _)| a.total_cmp(b));
        ExchangeRateLookup::Current {
            rate: current[current.len() / 2].0,
            expires_at,
        }
    }

    pub fn apply_event(&self, event: &EventEnvelope) {
        if event.ofs_spec != protocol::OFS_SPEC
            || event.event_type.as_str() != protocol::EVENT_PUBLISHED
        {
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
            payout_wallet: None,
            timestamp: Timestamp::now(),
        };
        services
            .apply_registration(SignedRegistration::sign(registration, &keypair))
            .unwrap();
        (keypair, services)
    }

    fn publish(provider: &Keypair, id: &str, version: u64, rate: f64) -> OraclePublish {
        OraclePublish {
            id: OracleId::new(id),
            provider: peer_id_from_public_key(&provider.public_key()).unwrap(),
            provider_public_key: provider.public_key(),
            data: OracleData::ExchangeRate {
                base: "USD".to_string(),
                quote: "KES".to_string(),
                rate,
            },
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
        let result = registry.apply_publish(SignedOraclePublish::sign(
            publish(&stranger, "usd-kes", 1, 129.52),
            &stranger,
        ));
        assert_eq!(result, Err(OracleError::Unauthorized));
    }

    #[test]
    fn a_registered_provider_can_publish_and_it_is_queryable() {
        let (provider, services) = registered_provider(1);
        let registry = OracleIndex::new(MemoryStore::new(), services);
        let id = registry
            .apply_publish(SignedOraclePublish::sign(
                publish(&provider, "usd-kes", 1, 129.52),
                &provider,
            ))
            .unwrap();
        assert_eq!(
            registry.get(&id).unwrap().data,
            OracleData::ExchangeRate {
                base: "USD".to_string(),
                quote: "KES".to_string(),
                rate: 129.52
            }
        );
    }

    #[test]
    fn a_stale_version_is_rejected() {
        let (provider, services) = registered_provider(1);
        let registry = OracleIndex::new(MemoryStore::new(), services);
        registry
            .apply_publish(SignedOraclePublish::sign(
                publish(&provider, "usd-kes", 2, 129.52),
                &provider,
            ))
            .unwrap();
        let result = registry.apply_publish(SignedOraclePublish::sign(
            publish(&provider, "usd-kes", 2, 129.60),
            &provider,
        ));
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
                payout_wallet: None,
                timestamp: Timestamp::now(),
            };
            services
                .apply_registration(SignedRegistration::sign(registration, &provider))
                .unwrap();
            registry
                .apply_publish(SignedOraclePublish::sign(
                    publish(&provider, &format!("usd-kes-{i}"), 1, *rate),
                    &provider,
                ))
                .unwrap();
        }
        assert_eq!(
            registry.median_exchange_rate("USD", "KES", Timestamp::now()),
            Some(129.51)
        );
    }

    #[test]
    fn an_expired_record_is_excluded_from_the_median() {
        let (provider, services) = registered_provider(1);
        let registry = OracleIndex::new(MemoryStore::new(), services);
        let mut record = publish(&provider, "usd-kes", 1, 129.52);
        record.expires_at = Timestamp::from_millis(record.timestamp.as_millis() + 1);
        registry
            .apply_publish(SignedOraclePublish::sign(record, &provider))
            .unwrap();

        let far_future = Timestamp::from_millis(Timestamp::now().as_millis() + 60_000);
        assert_eq!(
            registry.median_exchange_rate("USD", "KES", far_future),
            None
        );
    }

    /// The distinction `Option<f64>` cannot make: a pair nobody publishes
    /// versus a pair whose every publisher has lapsed. A caller pricing a
    /// trade must not treat the second as the first.
    #[test]
    fn a_lapsed_feed_reads_as_stale_not_as_an_unsupported_pair() {
        let (provider, services) = registered_provider(1);
        let registry = OracleIndex::new(MemoryStore::new(), services);
        let mut record = publish(&provider, "usd-kes", 1, 129.52);
        record.expires_at = Timestamp::from_millis(record.timestamp.as_millis() + 1);
        registry
            .apply_publish(SignedOraclePublish::sign(record, &provider))
            .unwrap();

        let far_future = Timestamp::from_millis(Timestamp::now().as_millis() + 60_000);
        assert_eq!(
            registry.exchange_rate("USD", "KES", far_future),
            ExchangeRateLookup::Stale
        );
        assert_eq!(
            registry.exchange_rate("USD", "NGN", far_future),
            ExchangeRateLookup::NoData,
            "a pair with no record at all is not the same failure as a lapsed one"
        );
    }

    /// A median is only as good as its shortest-lived contributor: once
    /// that record lapses the median is assembled from a different set.
    #[test]
    fn a_current_rate_expires_with_its_earliest_contributor() {
        let services = Rc::new(Registry::new(MemoryStore::new()));
        let registry = OracleIndex::new(MemoryStore::new(), Rc::clone(&services));
        let base = Timestamp::now().as_millis();

        for (i, (rate, ttl)) in [(129.50, 90_000u64), (129.54, 30_000), (129.51, 60_000)]
            .iter()
            .enumerate()
        {
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
                payout_wallet: None,
                timestamp: Timestamp::now(),
            };
            services
                .apply_registration(SignedRegistration::sign(registration, &provider))
                .unwrap();
            let mut record = publish(&provider, &format!("usd-kes-{i}"), 1, *rate);
            record.expires_at = Timestamp::from_millis(base + ttl);
            registry
                .apply_publish(SignedOraclePublish::sign(record, &provider))
                .unwrap();
        }

        assert_eq!(
            registry.exchange_rate("USD", "KES", Timestamp::from_millis(base)),
            ExchangeRateLookup::Current {
                rate: 129.51,
                expires_at: Timestamp::from_millis(base + 30_000),
            }
        );
    }

    /// Expiry and deletion are separate, and the gap between them is the
    /// point: an expired record still explains itself.
    mod pruning {
        use super::*;

        const DAY: u64 = 24 * 60 * 60 * 1_000;

        /// Publishes a live record — `apply_publish` refuses an
        /// already-expired one, which is why these tests move `now`
        /// forward rather than backdating the record.
        fn publish_live(
            registry: &OracleIndex<MemoryStore>,
            provider: &Keypair,
            id: &str,
        ) -> Timestamp {
            let record = publish(provider, id, 1, 129.52);
            let expires_at = record.expires_at;
            registry
                .apply_publish(SignedOraclePublish::sign(record, provider))
                .expect("a registered provider may publish a live record");
            expires_at
        }

        #[test]
        fn a_recently_expired_record_is_kept_so_it_can_explain_itself() {
            // "This feed died 25 hours ago" is a better answer than
            // silence — it is how #140 was diagnosed at all.
            let (provider, services) = registered_provider(1);
            let registry = OracleIndex::new(MemoryStore::new(), services);
            let expires_at = publish_live(&registry, &provider, "recent");

            let two_days_later = Timestamp::from_millis(expires_at.as_millis() + 2 * DAY);
            assert_eq!(registry.prune_expired(two_days_later), 0);
            assert_eq!(registry.all().len(), 1);
        }

        #[test]
        fn a_long_expired_record_is_deleted() {
            let (provider, services) = registered_provider(2);
            let registry = OracleIndex::new(MemoryStore::new(), services);
            let expires_at = publish_live(&registry, &provider, "ancient");

            let a_month_later = Timestamp::from_millis(expires_at.as_millis() + 30 * DAY);
            assert_eq!(registry.prune_expired(a_month_later), 1);
            assert!(registry.all().is_empty());
        }

        #[test]
        fn a_live_record_is_never_touched() {
            let (provider, services) = registered_provider(3);
            let registry = OracleIndex::new(MemoryStore::new(), services);
            publish_live(&registry, &provider, "live");

            assert_eq!(registry.prune_expired(Timestamp::now()), 0);
            assert_eq!(registry.all().len(), 1);
        }

        #[test]
        fn pruning_is_idempotent() {
            let (provider, services) = registered_provider(4);
            let registry = OracleIndex::new(MemoryStore::new(), services);
            let expires_at = publish_live(&registry, &provider, "ancient");
            let later = Timestamp::from_millis(expires_at.as_millis() + 30 * DAY);

            assert_eq!(registry.prune_expired(later), 1);
            assert_eq!(registry.prune_expired(later), 0);
        }
    }
}
