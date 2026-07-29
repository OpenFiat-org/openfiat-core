//! The replicated local risk index, sharing a handle to the node's
//! service registry (§5/§19: only a registered risk intelligence
//! provider may publish).

use crate::error::RiskError;
use crate::events::SignedRiskPublish;
use crate::protocol;
use crate::record::{RiskOutcome, RiskRecord, RiskRecordId, ScreeningResult};
use openfiat_registry::Registry;
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{EventEnvelope, PeerId, SecurityService, ServiceType, Timestamp};
use std::rc::Rc;

const COLUMN_FAMILY: &str = "risk_records";

pub struct RiskIndex<S> {
    store: S,
    services: Rc<Registry<S>>,
}

impl<S: KvStore> RiskIndex<S> {
    pub fn new(store: S, services: Rc<Registry<S>>) -> Self {
        Self { store, services }
    }

    pub fn get(&self, id: &RiskRecordId) -> Option<RiskRecord> {
        let bytes = self
            .store
            .get(COLUMN_FAMILY, id.as_str().as_bytes())
            .ok()
            .flatten()?;
        wire::from_bytes(&bytes).ok()
    }

    fn put(&self, record: &RiskRecord) {
        if let Ok(bytes) = wire::to_bytes(record) {
            let _ = self
                .store
                .put(COLUMN_FAMILY, record.id.as_str().as_bytes(), &bytes);
        }
    }

    pub fn all(&self) -> Vec<RiskRecord> {
        self.store
            .iter_prefix(COLUMN_FAMILY, &[])
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(_, value)| wire::from_bytes(&value).ok())
            .collect()
    }

    pub fn for_wallet(&self, wallet: &PeerId) -> Vec<RiskRecord> {
        self.all()
            .into_iter()
            .filter(|record| &record.wallet == wallet)
            .collect()
    }

    /// §5/§8/§19: only a registered risk intelligence provider may
    /// publish, and every record is permanent — publishing under an
    /// already-used Risk Record ID is rejected rather than overwriting
    /// history (§14).
    pub fn apply_publish(&self, signed: SignedRiskPublish) -> Result<RiskRecordId, RiskError> {
        signed.verify()?;
        let publish = signed.publish;
        if !self.services.all().into_iter().any(|service| {
            service.provider == publish.provider
                && matches!(
                    service.service_type,
                    ServiceType::Security(SecurityService::RiskIntelligenceProvider)
                )
        }) {
            return Err(RiskError::Unauthorized);
        }
        if self.get(&publish.id).is_some() {
            return Err(RiskError::DuplicateRecordId);
        }

        self.put(&RiskRecord {
            id: publish.id.clone(),
            provider: publish.provider,
            provider_public_key: publish.provider_public_key,
            wallet: publish.wallet,
            category: publish.category,
            outcome: publish.outcome,
            severity: publish.severity,
            confidence: publish.confidence,
            reason: publish.reason,
            evidence: publish.evidence,
            published_at: publish.timestamp,
            expires_at: publish.expires_at,
        });
        Ok(publish.id)
    }

    /// §11/§13/§14: the wallet-screening aggregation step. A `Cleared`
    /// record supersedes every `Flagged` record published before it;
    /// the aggregate severity is the worst among whatever `Flagged`
    /// records remain unsuperseded and current.
    pub fn screen(&self, wallet: &PeerId, now: Timestamp) -> ScreeningResult {
        let mut records: Vec<RiskRecord> = self
            .for_wallet(wallet)
            .into_iter()
            .filter(|record| record.is_current(now))
            .collect();
        records.sort_by_key(|record| record.published_at.as_millis());

        let cleared_at = records
            .iter()
            .rev()
            .find(|record| record.outcome == RiskOutcome::Cleared)
            .map(|record| record.published_at.as_millis());
        let active_flags: Vec<RiskRecord> = records
            .into_iter()
            .filter(|record| {
                record.outcome == RiskOutcome::Flagged
                    && cleared_at.is_none_or(|cleared| record.published_at.as_millis() > cleared)
            })
            .collect();
        let highest_severity = active_flags.iter().map(|record| record.severity).max();
        ScreeningResult {
            wallet: wallet.clone(),
            highest_severity,
            active_flags,
        }
    }

    pub fn apply_event(&self, event: &EventEnvelope) {
        if event.ofs_spec != protocol::OFS_SPEC {
            return;
        }
        if event.event_type.as_str() != protocol::EVENT_FLAGGED
            && event.event_type.as_str() != protocol::EVENT_CLEARED
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
    use crate::events::RiskPublish;
    use crate::record::{Confidence, ProviderCategory, Severity};
    use openfiat_crypto::Keypair;
    use openfiat_network::identity::peer_id_from_public_key;
    use openfiat_registry::{Registration, SignedRegistration};
    use openfiat_storage::mem::MemoryStore;
    use openfiat_types::ServiceId;

    fn registered_provider(seed: u8) -> (Keypair, Rc<Registry<MemoryStore>>) {
        let keypair = Keypair::from_seed([seed; 32]);
        let services = Rc::new(Registry::new(MemoryStore::new()));
        let registration = Registration {
            service_id: ServiceId::new(format!("risk-svc-{seed}")),
            service_type: ServiceType::Security(SecurityService::RiskIntelligenceProvider),
            provider: peer_id_from_public_key(&keypair.public_key()).unwrap(),
            provider_public_key: keypair.public_key(),
            endpoints: vec![],
            supported_ofs: vec![7100],
            region: None,
            capabilities: vec![],
            pricing: None,
            payout_wallet: None,
            timestamp: Timestamp::now(),
        };
        services
            .apply_registration(SignedRegistration::sign(registration, &keypair))
            .unwrap();
        (keypair, services)
    }

    fn flag(
        provider: &Keypair,
        id: &str,
        wallet: &PeerId,
        severity: Severity,
        published_at: Timestamp,
    ) -> RiskPublish {
        RiskPublish {
            id: RiskRecordId::new(id),
            provider: peer_id_from_public_key(&provider.public_key()).unwrap(),
            provider_public_key: provider.public_key(),
            wallet: wallet.clone(),
            category: ProviderCategory::FraudIntelligence,
            outcome: RiskOutcome::Flagged,
            severity,
            confidence: Confidence::High,
            reason: "Known scam wallet".to_string(),
            evidence: vec![],
            timestamp: published_at,
            expires_at: None,
        }
    }

    #[test]
    fn an_unregistered_publisher_is_rejected() {
        let services = Rc::new(Registry::new(MemoryStore::new()));
        let registry = RiskIndex::new(MemoryStore::new(), services);
        let stranger = Keypair::generate();
        let wallet = peer_id_from_public_key(&Keypair::generate().public_key()).unwrap();
        let result = registry.apply_publish(SignedRiskPublish::sign(
            flag(&stranger, "r1", &wallet, Severity::High, Timestamp::now()),
            &stranger,
        ));
        assert_eq!(result, Err(RiskError::Unauthorized));
    }

    #[test]
    fn a_duplicate_record_id_is_rejected() {
        let (provider, services) = registered_provider(1);
        let registry = RiskIndex::new(MemoryStore::new(), services);
        let wallet = peer_id_from_public_key(&Keypair::generate().public_key()).unwrap();
        registry
            .apply_publish(SignedRiskPublish::sign(
                flag(&provider, "r1", &wallet, Severity::High, Timestamp::now()),
                &provider,
            ))
            .unwrap();
        let result = registry.apply_publish(SignedRiskPublish::sign(
            flag(&provider, "r1", &wallet, Severity::Low, Timestamp::now()),
            &provider,
        ));
        assert_eq!(result, Err(RiskError::DuplicateRecordId));
    }

    #[test]
    fn screening_aggregates_to_the_worst_severity_among_current_flags() {
        let (provider, services) = registered_provider(1);
        let registry = RiskIndex::new(MemoryStore::new(), services);
        let wallet = peer_id_from_public_key(&Keypair::generate().public_key()).unwrap();
        registry
            .apply_publish(SignedRiskPublish::sign(
                flag(
                    &provider,
                    "r1",
                    &wallet,
                    Severity::Medium,
                    Timestamp::from_millis(1),
                ),
                &provider,
            ))
            .unwrap();
        registry
            .apply_publish(SignedRiskPublish::sign(
                flag(
                    &provider,
                    "r2",
                    &wallet,
                    Severity::Critical,
                    Timestamp::from_millis(2),
                ),
                &provider,
            ))
            .unwrap();

        let result = registry.screen(&wallet, Timestamp::now());
        assert_eq!(result.highest_severity, Some(Severity::Critical));
        assert_eq!(result.active_flags.len(), 2);
    }

    #[test]
    fn a_later_clearance_supersedes_earlier_flags() {
        let (provider, services) = registered_provider(1);
        let registry = RiskIndex::new(MemoryStore::new(), services);
        let wallet = peer_id_from_public_key(&Keypair::generate().public_key()).unwrap();
        registry
            .apply_publish(SignedRiskPublish::sign(
                flag(
                    &provider,
                    "r1",
                    &wallet,
                    Severity::Critical,
                    Timestamp::from_millis(1),
                ),
                &provider,
            ))
            .unwrap();

        let clear = RiskPublish {
            outcome: RiskOutcome::Cleared,
            severity: Severity::Informational,
            confidence: Confidence::VeryHigh,
            reason: "Investigation closed, false positive".to_string(),
            timestamp: Timestamp::from_millis(2),
            ..flag(
                &provider,
                "r2",
                &wallet,
                Severity::Informational,
                Timestamp::from_millis(2),
            )
        };
        registry
            .apply_publish(SignedRiskPublish::sign(clear, &provider))
            .unwrap();

        let result = registry.screen(&wallet, Timestamp::now());
        assert_eq!(result.highest_severity, None);
        assert!(result.active_flags.is_empty());
    }

    #[test]
    fn a_flag_after_a_clearance_is_not_superseded() {
        let (provider, services) = registered_provider(1);
        let registry = RiskIndex::new(MemoryStore::new(), services);
        let wallet = peer_id_from_public_key(&Keypair::generate().public_key()).unwrap();

        let clear = RiskPublish {
            outcome: RiskOutcome::Cleared,
            timestamp: Timestamp::from_millis(1),
            ..flag(
                &provider,
                "r1",
                &wallet,
                Severity::Informational,
                Timestamp::from_millis(1),
            )
        };
        registry
            .apply_publish(SignedRiskPublish::sign(clear, &provider))
            .unwrap();
        registry
            .apply_publish(SignedRiskPublish::sign(
                flag(
                    &provider,
                    "r2",
                    &wallet,
                    Severity::High,
                    Timestamp::from_millis(2),
                ),
                &provider,
            ))
            .unwrap();

        let result = registry.screen(&wallet, Timestamp::now());
        assert_eq!(result.highest_severity, Some(Severity::High));
    }
}
