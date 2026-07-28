//! The replicated local notification index, sharing a handle to the
//! node's service registry (§17: a delivery report is only accepted from
//! whichever peer `openfiat-registry` has on file as that service's
//! provider — not a self-asserted identity).

use crate::error::NotificationError;
use crate::events::{SignedDeliveryReport, SignedSubscriptionUpdate};
use crate::protocol;
use crate::record::{DeliveryReceipt, NotificationId, Subscription};
use openfiat_registry::Registry;
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{EventEnvelope, PeerId};
use std::rc::Rc;

const SUBSCRIPTIONS_COLUMN_FAMILY: &str = "notification_subscriptions";
const RECEIPTS_COLUMN_FAMILY: &str = "notification_receipts";

pub struct NotificationRegistry<S> {
    store: S,
    services: Rc<Registry<S>>,
}

impl<S: KvStore> NotificationRegistry<S> {
    pub fn new(store: S, services: Rc<Registry<S>>) -> Self {
        Self { store, services }
    }

    pub fn subscription(&self, wallet: &PeerId) -> Option<Subscription> {
        let bytes = self
            .store
            .get(SUBSCRIPTIONS_COLUMN_FAMILY, wallet.as_bytes())
            .ok()
            .flatten()?;
        wire::from_bytes(&bytes).ok()
    }

    pub fn all_subscriptions(&self) -> Vec<Subscription> {
        self.store
            .iter_prefix(SUBSCRIPTIONS_COLUMN_FAMILY, &[])
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(_, value)| wire::from_bytes(&value).ok())
            .collect()
    }

    pub fn receipt(&self, id: &NotificationId) -> Option<DeliveryReceipt> {
        let bytes = self
            .store
            .get(RECEIPTS_COLUMN_FAMILY, id.as_str().as_bytes())
            .ok()
            .flatten()?;
        wire::from_bytes(&bytes).ok()
    }

    pub fn receipts_for(&self, wallet: &PeerId) -> Vec<DeliveryReceipt> {
        self.store
            .iter_prefix(RECEIPTS_COLUMN_FAMILY, &[])
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(_, value)| wire::from_bytes::<DeliveryReceipt>(&value).ok())
            .filter(|receipt| &receipt.recipient_wallet == wallet)
            .collect()
    }

    /// §11: a full upsert — the latest update fully replaces whatever
    /// this wallet's subscription previously was.
    pub fn apply_subscription_update(
        &self,
        signed: SignedSubscriptionUpdate,
    ) -> Result<(), NotificationError> {
        signed.verify()?;
        let update = signed.update;
        let subscription = Subscription {
            wallet: update.wallet.clone(),
            wallet_public_key: update.wallet_public_key,
            enabled_categories: update.enabled_categories,
            updated_at: update.timestamp,
        };
        let bytes = wire::to_bytes(&subscription).map_err(|_| NotificationError::MalformedEvent)?;
        let _ = self.store.put(
            SUBSCRIPTIONS_COLUMN_FAMILY,
            update.wallet.as_bytes(),
            &bytes,
        );
        Ok(())
    }

    /// §17/§20: only a service's on-file provider (per `openfiat-registry`)
    /// may report a delivery outcome for it.
    pub fn apply_delivery_report(
        &self,
        signed: SignedDeliveryReport,
    ) -> Result<(), NotificationError> {
        signed.verify()?;
        let service = self
            .services
            .get(&signed.report.service_id)
            .ok_or(NotificationError::ServiceNotFound)?;
        if service.provider != signed.report.provider {
            return Err(NotificationError::Unauthorized);
        }

        let report = signed.report;
        let receipt = DeliveryReceipt {
            notification_id: report.notification_id.clone(),
            service_id: report.service_id,
            recipient_wallet: report.recipient_wallet,
            trigger: report.trigger,
            status: report.status,
            updated_at: report.timestamp,
        };
        let bytes = wire::to_bytes(&receipt).map_err(|_| NotificationError::MalformedEvent)?;
        let _ = self.store.put(
            RECEIPTS_COLUMN_FAMILY,
            report.notification_id.as_str().as_bytes(),
            &bytes,
        );
        Ok(())
    }

    pub fn apply_event(&self, event: &EventEnvelope) {
        if event.ofs_spec != protocol::OFS_SPEC {
            return;
        }
        if event.event_type.as_str() == protocol::EVENT_SUBSCRIPTION_UPDATED {
            if let Ok(signed) = wire::from_bytes(&event.payload) {
                let _ = self.apply_subscription_update(signed);
            }
            return;
        }
        if protocol::DELIVERY_EVENT_NAMES.contains(&event.event_type.as_str())
            && let Ok(signed) = wire::from_bytes(&event.payload)
        {
            let _ = self.apply_delivery_report(signed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{DeliveryReport, SubscriptionUpdate};
    use crate::record::{DeliveryStatus, NotificationCategory, NotificationTrigger};
    use openfiat_crypto::Keypair;
    use openfiat_network::identity::peer_id_from_public_key;
    use openfiat_registry::{Registration, SignedRegistration};
    use openfiat_storage::mem::MemoryStore;
    use openfiat_types::{NotificationChannel, ServiceId, ServiceType, Timestamp};

    fn setup_with_provider(
        provider: &Keypair,
        service_id: &str,
    ) -> NotificationRegistry<MemoryStore> {
        let services = Rc::new(Registry::new(MemoryStore::new()));
        let registration = Registration {
            service_id: ServiceId::new(service_id),
            service_type: ServiceType::Notifications(NotificationChannel::Sms),
            provider: peer_id_from_public_key(&provider.public_key()).unwrap(),
            provider_public_key: provider.public_key(),
            endpoints: vec!["https://sms.example/webhook".to_string()],
            supported_ofs: vec![6000],
            region: Some("Kenya".to_string()),
            capabilities: vec![],
            pricing: None,
            timestamp: Timestamp::now(),
        };
        services
            .apply_registration(SignedRegistration::sign(registration, provider))
            .unwrap();
        NotificationRegistry::new(MemoryStore::new(), services)
    }

    #[test]
    fn a_subscription_update_is_queryable_by_wallet() {
        let services = Rc::new(Registry::new(MemoryStore::new()));
        let registry = NotificationRegistry::new(MemoryStore::new(), services);
        let wallet = Keypair::generate();
        let update = SubscriptionUpdate {
            wallet: peer_id_from_public_key(&wallet.public_key()).unwrap(),
            wallet_public_key: wallet.public_key(),
            enabled_categories: vec![
                NotificationCategory::Trading,
                NotificationCategory::Governance,
            ],
            timestamp: Timestamp::now(),
        };
        registry
            .apply_subscription_update(SignedSubscriptionUpdate::sign(update, &wallet))
            .unwrap();

        let wallet_id = peer_id_from_public_key(&wallet.public_key()).unwrap();
        let subscription = registry.subscription(&wallet_id).unwrap();
        assert!(subscription.wants(NotificationTrigger::TradeCompleted));
        assert!(!subscription.wants(NotificationTrigger::SnapshotAvailable));
    }

    #[test]
    fn a_later_subscription_update_fully_replaces_the_earlier_one() {
        let services = Rc::new(Registry::new(MemoryStore::new()));
        let registry = NotificationRegistry::new(MemoryStore::new(), services);
        let wallet = Keypair::generate();
        let wallet_id = peer_id_from_public_key(&wallet.public_key()).unwrap();
        let first = SubscriptionUpdate {
            wallet: wallet_id.clone(),
            wallet_public_key: wallet.public_key(),
            enabled_categories: vec![NotificationCategory::Trading],
            timestamp: Timestamp::now(),
        };
        registry
            .apply_subscription_update(SignedSubscriptionUpdate::sign(first, &wallet))
            .unwrap();

        let second = SubscriptionUpdate {
            wallet: wallet_id.clone(),
            wallet_public_key: wallet.public_key(),
            enabled_categories: vec![NotificationCategory::Infrastructure],
            timestamp: Timestamp::now(),
        };
        registry
            .apply_subscription_update(SignedSubscriptionUpdate::sign(second, &wallet))
            .unwrap();

        let subscription = registry.subscription(&wallet_id).unwrap();
        assert!(!subscription.wants(NotificationTrigger::TradeCompleted));
        assert!(subscription.wants(NotificationTrigger::SnapshotAvailable));
    }

    #[test]
    fn a_delivery_report_from_the_registered_provider_is_accepted() {
        let provider = Keypair::generate();
        let registry = setup_with_provider(&provider, "svc-1");
        let recipient = Keypair::generate();
        let report = DeliveryReport {
            notification_id: NotificationId::new("notif-1"),
            service_id: ServiceId::new("svc-1"),
            provider: peer_id_from_public_key(&provider.public_key()).unwrap(),
            provider_public_key: provider.public_key(),
            recipient_wallet: peer_id_from_public_key(&recipient.public_key()).unwrap(),
            trigger: NotificationTrigger::TradeCompleted,
            status: DeliveryStatus::Delivered,
            timestamp: Timestamp::now(),
        };
        registry
            .apply_delivery_report(SignedDeliveryReport::sign(report, &provider))
            .unwrap();
        assert_eq!(
            registry
                .receipt(&NotificationId::new("notif-1"))
                .unwrap()
                .status,
            DeliveryStatus::Delivered
        );
    }

    #[test]
    fn a_delivery_report_from_an_impostor_is_rejected() {
        let provider = Keypair::generate();
        let registry = setup_with_provider(&provider, "svc-1");
        let impostor = Keypair::generate();
        let recipient = Keypair::generate();
        let report = DeliveryReport {
            notification_id: NotificationId::new("notif-1"),
            service_id: ServiceId::new("svc-1"),
            provider: peer_id_from_public_key(&impostor.public_key()).unwrap(),
            provider_public_key: impostor.public_key(),
            recipient_wallet: peer_id_from_public_key(&recipient.public_key()).unwrap(),
            trigger: NotificationTrigger::TradeCompleted,
            status: DeliveryStatus::Delivered,
            timestamp: Timestamp::now(),
        };
        let result = registry.apply_delivery_report(SignedDeliveryReport::sign(report, &impostor));
        assert_eq!(result, Err(NotificationError::Unauthorized));
    }

    #[test]
    fn a_delivery_report_for_an_unregistered_service_is_rejected() {
        let services = Rc::new(Registry::new(MemoryStore::new()));
        let registry = NotificationRegistry::new(MemoryStore::new(), services);
        let provider = Keypair::generate();
        let recipient = Keypair::generate();
        let report = DeliveryReport {
            notification_id: NotificationId::new("notif-1"),
            service_id: ServiceId::new("svc-unknown"),
            provider: peer_id_from_public_key(&provider.public_key()).unwrap(),
            provider_public_key: provider.public_key(),
            recipient_wallet: peer_id_from_public_key(&recipient.public_key()).unwrap(),
            trigger: NotificationTrigger::TradeCompleted,
            status: DeliveryStatus::Delivered,
            timestamp: Timestamp::now(),
        };
        let result = registry.apply_delivery_report(SignedDeliveryReport::sign(report, &provider));
        assert_eq!(result, Err(NotificationError::ServiceNotFound));
    }
}
