//! The replicated local notification index, sharing a handle to the
//! node's service registry (§17: a delivery report is only accepted from
//! whichever peer `openfiat-registry` has on file as that service's
//! provider — not a self-asserted identity).

use crate::error::NotificationError;
use crate::events::{SignedDeliveryReport, SignedSubscriptionUpdate};
use crate::protocol;
use crate::record::{
    DeliveryReceipt, DeliveryStatus, DispatchRecord, NotificationId, NotificationTrigger,
    Subscription,
};
use crate::routing::{self, PlannedDelivery, RoutingPlan};
use openfiat_registry::Registry;
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{EventEnvelope, PeerId, Timestamp};
use std::rc::Rc;

const SUBSCRIPTIONS_COLUMN_FAMILY: &str = "notification_subscriptions";
const RECEIPTS_COLUMN_FAMILY: &str = "notification_receipts";
/// What this node itself dispatched, as opposed to what a gateway later
/// claimed about it — see [`DispatchRecord`].
const DISPATCHES_COLUMN_FAMILY: &str = "notification_dispatches";

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
            destinations: update.destinations,
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
    /// may report a delivery outcome — and, now, only for a notification
    /// this node actually routed *to that service*.
    ///
    /// Three tiers, in order:
    ///
    /// 1. The report is signed by the peer it claims to come from.
    /// 2. That peer is the on-file provider for the referenced service.
    /// 3. This node has a [`DispatchRecord`] for the notification id, and
    ///    it names the same service, recipient, and trigger.
    ///
    /// Tier 3 is what stops two distinct abuses that tiers 1 and 2 leave
    /// open. Any *registered* gateway could otherwise report on deliveries
    /// it was never routed (inflating its own operational record and
    /// smearing a competitor's), and any gateway could invent a
    /// notification id outright, conjuring a receipt for a message no
    /// node ever asked anyone to send.
    ///
    /// The cost is real and worth stating: a node that never routed a
    /// given notification — because it joined late, restored from a
    /// snapshot, or held no replica of that wallet's subscription when
    /// the source event passed — legitimately does not know the id, and
    /// will reject a perfectly honest report as
    /// [`NotificationError::UnknownNotification`]. That is the deliberate
    /// choice: dropping a report a node cannot check is recoverable (the
    /// nodes that *did* route it still accept and gossip it, and
    /// dispatch is deterministic, so in steady state that is every node),
    /// whereas accepting an uncheckable one writes an unverifiable claim
    /// into replicated state permanently.
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
        let dispatched = self
            .dispatch(&signed.report.notification_id)
            .ok_or(NotificationError::UnknownNotification)?;
        if dispatched.service_id != signed.report.service_id
            || dispatched.recipient_wallet != signed.report.recipient_wallet
            || dispatched.trigger != signed.report.trigger
        {
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

    /// What this node itself dispatched under `id`, if anything.
    pub fn dispatch(&self, id: &NotificationId) -> Option<DispatchRecord> {
        let bytes = self
            .store
            .get(DISPATCHES_COLUMN_FAMILY, id.as_str().as_bytes())
            .ok()
            .flatten()?;
        wire::from_bytes(&bytes).ok()
    }

    pub fn dispatches_for(&self, wallet: &PeerId) -> Vec<DispatchRecord> {
        self.store
            .iter_prefix(DISPATCHES_COLUMN_FAMILY, &[])
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(_, value)| wire::from_bytes::<DispatchRecord>(&value).ok())
            .filter(|record| &record.recipient_wallet == wallet)
            .collect()
    }

    /// Plan every delivery `trigger` should produce for `recipient`,
    /// against this node's own replica of the recipient's subscription
    /// and the service registry.
    ///
    /// A wallet with no subscription here yields an empty plan rather
    /// than an error: not having opted in is the normal case, not a
    /// failure.
    pub fn plan(
        &self,
        trigger: NotificationTrigger,
        source_event: &[u8],
        recipient: &PeerId,
    ) -> RoutingPlan {
        match self.subscription(recipient) {
            Some(subscription) => {
                routing::plan(&self.services, &subscription, trigger, source_event)
            }
            None => RoutingPlan::default(),
        }
    }

    /// Record that `delivery` is about to be attempted (`Queued`).
    ///
    /// Written *before* the handoff, not after, for two reasons: a report
    /// arriving between the POST and its response must still be
    /// checkable, and a crash mid-handoff should leave evidence that the
    /// attempt happened rather than none.
    pub fn record_queued(&self, delivery: &PlannedDelivery) {
        self.write_dispatch(&DispatchRecord {
            notification_id: delivery.payload.notification_id.clone(),
            service_id: delivery.payload.service_id.clone(),
            recipient_wallet: delivery.payload.recipient_wallet.clone(),
            trigger: delivery.payload.trigger,
            channel: delivery.payload.channel,
            status: DeliveryStatus::Queued,
            updated_at: Timestamp::now(),
        });
    }

    /// Record the outcome of the handoff itself: `Sent` if the gateway
    /// accepted the payload, `Failed` if it could not be reached or
    /// refused it.
    ///
    /// This is the furthest a node's own observation can honestly go. It
    /// never advances to `Delivered` or `Read` — those are last-mile
    /// facts only the gateway sees, and they arrive as a signed
    /// `DeliveryReport` into the separate receipts family.
    pub fn record_handoff(&self, id: &NotificationId, accepted: bool) {
        let Some(mut record) = self.dispatch(id) else {
            return;
        };
        record.status = if accepted {
            DeliveryStatus::Sent
        } else {
            DeliveryStatus::Failed
        };
        record.updated_at = Timestamp::now();
        self.write_dispatch(&record);
    }

    fn write_dispatch(&self, record: &DispatchRecord) {
        let Ok(bytes) = wire::to_bytes(record) else {
            return;
        };
        let _ = self.store.put(
            DISPATCHES_COLUMN_FAMILY,
            record.notification_id.as_str().as_bytes(),
            &bytes,
        );
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
    use crate::record::{
        DeliveryStatus, NotificationCategory, NotificationTrigger, SubscriptionDestination,
    };
    use openfiat_crypto::{Keypair, seal};
    use openfiat_network::identity::peer_id_from_public_key;
    use openfiat_registry::{Registration, SignedRegistration};
    use openfiat_storage::mem::MemoryStore;
    use openfiat_types::{NotificationChannel, ServiceId, ServiceType, Timestamp};

    fn register(services: &Registry<MemoryStore>, provider: &Keypair, service_id: &str) {
        let registration = Registration {
            service_id: ServiceId::new(service_id),
            service_type: ServiceType::Notifications(NotificationChannel::Sms),
            provider: peer_id_from_public_key(&provider.public_key()).unwrap(),
            provider_public_key: provider.public_key(),
            endpoints: vec!["https://sms.example.com/webhook".to_string()],
            supported_ofs: vec![6000],
            region: Some("Kenya".to_string()),
            capabilities: vec![],
            branding: None,
            pricing: None,
            payout_wallet: None,
            timestamp: Timestamp::now(),
        };
        services
            .apply_registration(SignedRegistration::sign(registration, provider))
            .unwrap();
    }

    fn setup_with_provider(
        provider: &Keypair,
        service_id: &str,
    ) -> NotificationRegistry<MemoryStore> {
        let services = Rc::new(Registry::new(MemoryStore::new()));
        register(&services, provider, service_id);
        NotificationRegistry::new(MemoryStore::new(), services)
    }

    fn subscribe(
        registry: &NotificationRegistry<MemoryStore>,
        wallet: &Keypair,
        categories: Vec<NotificationCategory>,
        destinations: Vec<SubscriptionDestination>,
    ) -> PeerId {
        let wallet_id = peer_id_from_public_key(&wallet.public_key()).unwrap();
        let update = SubscriptionUpdate {
            wallet: wallet_id.clone(),
            wallet_public_key: wallet.public_key(),
            enabled_categories: categories,
            destinations,
            timestamp: Timestamp::now(),
        };
        registry
            .apply_subscription_update(SignedSubscriptionUpdate::sign(update, wallet))
            .unwrap();
        wallet_id
    }

    fn destination(gateway: &Keypair, service_id: &str) -> SubscriptionDestination {
        SubscriptionDestination {
            service_id: ServiceId::new(service_id),
            channel: NotificationChannel::Sms,
            sealed: seal(&gateway.public_key(), b"+254700000000").unwrap(),
        }
    }

    /// Plans and records one dispatch, returning its id — the precondition
    /// for any delivery report being accepted.
    fn dispatch_one(
        registry: &NotificationRegistry<MemoryStore>,
        wallet: &PeerId,
        trigger: NotificationTrigger,
    ) -> NotificationId {
        let plan = registry.plan(trigger, b"source-event", wallet);
        let delivery = plan
            .deliveries
            .first()
            .expect("the fixture gateway is registered, healthy, and bound");
        registry.record_queued(delivery);
        delivery.payload.notification_id.clone()
    }

    fn report(
        id: &NotificationId,
        service_id: &str,
        provider: &Keypair,
        wallet: &PeerId,
        trigger: NotificationTrigger,
        status: DeliveryStatus,
    ) -> DeliveryReport {
        DeliveryReport {
            notification_id: id.clone(),
            service_id: ServiceId::new(service_id),
            provider: peer_id_from_public_key(&provider.public_key()).unwrap(),
            provider_public_key: provider.public_key(),
            recipient_wallet: wallet.clone(),
            trigger,
            status,
            timestamp: Timestamp::now(),
        }
    }

    #[test]
    fn a_subscription_update_is_queryable_by_wallet() {
        let services = Rc::new(Registry::new(MemoryStore::new()));
        let registry = NotificationRegistry::new(MemoryStore::new(), services);
        let wallet = Keypair::generate();
        let wallet_id = subscribe(
            &registry,
            &wallet,
            vec![
                NotificationCategory::Trading,
                NotificationCategory::Governance,
            ],
            vec![],
        );

        let subscription = registry.subscription(&wallet_id).unwrap();
        assert!(subscription.wants(NotificationTrigger::TradeCompleted));
        assert!(!subscription.wants(NotificationTrigger::SnapshotAvailable));
    }

    #[test]
    fn a_subscription_update_carries_its_sealed_destinations_into_the_store() {
        let gateway = Keypair::generate();
        let registry = setup_with_provider(&gateway, "svc-1");
        let wallet = Keypair::generate();
        let wallet_id = subscribe(
            &registry,
            &wallet,
            vec![NotificationCategory::Trading],
            vec![destination(&gateway, "svc-1")],
        );

        let stored = registry.subscription(&wallet_id).unwrap();
        assert_eq!(stored.destinations.len(), 1);
        assert_eq!(
            openfiat_crypto::open(&gateway, &stored.destinations[0].sealed).unwrap(),
            b"+254700000000",
            "the destination survives the round trip, readable only by the gateway"
        );
    }

    #[test]
    fn a_later_subscription_update_fully_replaces_the_earlier_one() {
        let registry = NotificationRegistry::new(
            MemoryStore::new(),
            Rc::new(Registry::new(MemoryStore::new())),
        );
        let wallet = Keypair::generate();
        subscribe(
            &registry,
            &wallet,
            vec![NotificationCategory::Trading],
            vec![],
        );
        let wallet_id = subscribe(
            &registry,
            &wallet,
            vec![NotificationCategory::Infrastructure],
            vec![],
        );

        let subscription = registry.subscription(&wallet_id).unwrap();
        assert!(!subscription.wants(NotificationTrigger::TradeCompleted));
        assert!(subscription.wants(NotificationTrigger::SnapshotAvailable));
    }

    #[test]
    fn planning_a_trigger_for_an_unknown_wallet_is_empty_not_an_error() {
        let registry = NotificationRegistry::new(
            MemoryStore::new(),
            Rc::new(Registry::new(MemoryStore::new())),
        );
        let plan = registry.plan(
            NotificationTrigger::TradeCompleted,
            b"source-event",
            &PeerId::from_bytes(b"never-subscribed".to_vec()),
        );
        assert!(plan.is_empty());
    }

    #[test]
    fn a_dispatch_moves_from_queued_to_sent_on_a_successful_handoff() {
        let gateway = Keypair::generate();
        let registry = setup_with_provider(&gateway, "svc-1");
        let wallet = Keypair::generate();
        let wallet_id = subscribe(
            &registry,
            &wallet,
            vec![NotificationCategory::Trading],
            vec![destination(&gateway, "svc-1")],
        );

        let id = dispatch_one(&registry, &wallet_id, NotificationTrigger::TradeCompleted);
        assert_eq!(
            registry.dispatch(&id).unwrap().status,
            DeliveryStatus::Queued
        );
        registry.record_handoff(&id, true);
        assert_eq!(registry.dispatch(&id).unwrap().status, DeliveryStatus::Sent);
        assert_eq!(registry.dispatches_for(&wallet_id).len(), 1);
    }

    #[test]
    fn a_refused_handoff_is_recorded_as_failed() {
        let gateway = Keypair::generate();
        let registry = setup_with_provider(&gateway, "svc-1");
        let wallet = Keypair::generate();
        let wallet_id = subscribe(
            &registry,
            &wallet,
            vec![NotificationCategory::Trading],
            vec![destination(&gateway, "svc-1")],
        );

        let id = dispatch_one(&registry, &wallet_id, NotificationTrigger::TradeCompleted);
        registry.record_handoff(&id, false);
        assert_eq!(
            registry.dispatch(&id).unwrap().status,
            DeliveryStatus::Failed
        );
    }

    #[test]
    fn a_delivery_report_from_the_registered_provider_is_accepted() {
        let provider = Keypair::generate();
        let registry = setup_with_provider(&provider, "svc-1");
        let wallet = Keypair::generate();
        let wallet_id = subscribe(
            &registry,
            &wallet,
            vec![NotificationCategory::Trading],
            vec![destination(&provider, "svc-1")],
        );
        let id = dispatch_one(&registry, &wallet_id, NotificationTrigger::TradeCompleted);
        registry.record_handoff(&id, true);

        registry
            .apply_delivery_report(SignedDeliveryReport::sign(
                report(
                    &id,
                    "svc-1",
                    &provider,
                    &wallet_id,
                    NotificationTrigger::TradeCompleted,
                    DeliveryStatus::Delivered,
                ),
                &provider,
            ))
            .unwrap();

        assert_eq!(
            registry.receipt(&id).unwrap().status,
            DeliveryStatus::Delivered
        );
        assert_eq!(
            registry.dispatch(&id).unwrap().status,
            DeliveryStatus::Sent,
            "the gateway's last-mile claim lives beside, not on top of, the node's own record"
        );
    }

    /// The id the dispatcher derived is exactly the one a report must
    /// carry — that round trip is what lets N nodes and the gateway talk
    /// about the same notification at all.
    #[test]
    fn the_reported_id_is_the_one_the_dispatcher_derived() {
        let provider = Keypair::generate();
        let registry = setup_with_provider(&provider, "svc-1");
        let wallet = Keypair::generate();
        let wallet_id = subscribe(
            &registry,
            &wallet,
            vec![NotificationCategory::Trading],
            vec![destination(&provider, "svc-1")],
        );
        let id = dispatch_one(&registry, &wallet_id, NotificationTrigger::TradeCompleted);

        assert_eq!(
            id,
            NotificationId::derive(
                NotificationTrigger::TradeCompleted,
                b"source-event",
                &wallet_id
            )
        );
        registry
            .apply_delivery_report(SignedDeliveryReport::sign(
                report(
                    &id,
                    "svc-1",
                    &provider,
                    &wallet_id,
                    NotificationTrigger::TradeCompleted,
                    DeliveryStatus::Read,
                ),
                &provider,
            ))
            .unwrap();
        assert_eq!(registry.receipt(&id).unwrap().notification_id, id);
    }

    #[test]
    fn a_delivery_report_from_an_impostor_is_rejected() {
        let provider = Keypair::generate();
        let registry = setup_with_provider(&provider, "svc-1");
        let impostor = Keypair::generate();
        let wallet = Keypair::generate();
        let wallet_id = subscribe(
            &registry,
            &wallet,
            vec![NotificationCategory::Trading],
            vec![destination(&provider, "svc-1")],
        );
        let id = dispatch_one(&registry, &wallet_id, NotificationTrigger::TradeCompleted);

        let result = registry.apply_delivery_report(SignedDeliveryReport::sign(
            report(
                &id,
                "svc-1",
                &impostor,
                &wallet_id,
                NotificationTrigger::TradeCompleted,
                DeliveryStatus::Delivered,
            ),
            &impostor,
        ));
        assert_eq!(result, Err(NotificationError::Unauthorized));
    }

    #[test]
    fn a_delivery_report_for_an_unregistered_service_is_rejected() {
        let registry = NotificationRegistry::new(
            MemoryStore::new(),
            Rc::new(Registry::new(MemoryStore::new())),
        );
        let provider = Keypair::generate();
        let wallet_id = PeerId::from_bytes(b"wallet".to_vec());
        let result = registry.apply_delivery_report(SignedDeliveryReport::sign(
            report(
                &NotificationId::new("notif-1"),
                "svc-unknown",
                &provider,
                &wallet_id,
                NotificationTrigger::TradeCompleted,
                DeliveryStatus::Delivered,
            ),
            &provider,
        ));
        assert_eq!(result, Err(NotificationError::ServiceNotFound));
    }

    /// Without this, a gateway could mint receipts for notifications
    /// nobody ever asked it to deliver.
    #[test]
    fn a_delivery_report_for_a_notification_no_node_dispatched_is_rejected() {
        let provider = Keypair::generate();
        let registry = setup_with_provider(&provider, "svc-1");
        let wallet_id = PeerId::from_bytes(b"wallet".to_vec());

        let result = registry.apply_delivery_report(SignedDeliveryReport::sign(
            report(
                &NotificationId::new("fabricated"),
                "svc-1",
                &provider,
                &wallet_id,
                NotificationTrigger::TradeCompleted,
                DeliveryStatus::Delivered,
            ),
            &provider,
        ));

        assert_eq!(result, Err(NotificationError::UnknownNotification));
        assert_eq!(
            registry.receipt(&NotificationId::new("fabricated")),
            None,
            "a rejected report must leave no trace, not a half-written record"
        );
    }

    /// Registration alone must not be a licence to report on traffic that
    /// went somewhere else.
    #[test]
    fn a_registered_gateway_cannot_report_on_a_delivery_it_was_not_routed() {
        let bound_gateway = Keypair::generate();
        let other_gateway = Keypair::generate();
        let services = Rc::new(Registry::new(MemoryStore::new()));
        register(&services, &bound_gateway, "svc-bound");
        register(&services, &other_gateway, "svc-other");
        let registry = NotificationRegistry::new(MemoryStore::new(), services);

        let wallet = Keypair::generate();
        let wallet_id = subscribe(
            &registry,
            &wallet,
            vec![NotificationCategory::Trading],
            vec![destination(&bound_gateway, "svc-bound")],
        );
        let id = dispatch_one(&registry, &wallet_id, NotificationTrigger::TradeCompleted);

        let result = registry.apply_delivery_report(SignedDeliveryReport::sign(
            report(
                &id,
                "svc-other",
                &other_gateway,
                &wallet_id,
                NotificationTrigger::TradeCompleted,
                DeliveryStatus::Delivered,
            ),
            &other_gateway,
        ));

        assert_eq!(result, Err(NotificationError::Unauthorized));
        assert_eq!(registry.receipt(&id), None);
    }

    #[test]
    fn a_report_that_rewrites_the_recipient_is_rejected() {
        let provider = Keypair::generate();
        let registry = setup_with_provider(&provider, "svc-1");
        let wallet = Keypair::generate();
        let wallet_id = subscribe(
            &registry,
            &wallet,
            vec![NotificationCategory::Trading],
            vec![destination(&provider, "svc-1")],
        );
        let id = dispatch_one(&registry, &wallet_id, NotificationTrigger::TradeCompleted);

        let result = registry.apply_delivery_report(SignedDeliveryReport::sign(
            report(
                &id,
                "svc-1",
                &provider,
                &PeerId::from_bytes(b"somebody-else".to_vec()),
                NotificationTrigger::TradeCompleted,
                DeliveryStatus::Delivered,
            ),
            &provider,
        ));
        assert_eq!(result, Err(NotificationError::Unauthorized));
    }

    #[test]
    fn a_report_that_rewrites_the_trigger_is_rejected() {
        let provider = Keypair::generate();
        let registry = setup_with_provider(&provider, "svc-1");
        let wallet = Keypair::generate();
        let wallet_id = subscribe(
            &registry,
            &wallet,
            vec![NotificationCategory::Trading],
            vec![destination(&provider, "svc-1")],
        );
        let id = dispatch_one(&registry, &wallet_id, NotificationTrigger::TradeCompleted);

        let result = registry.apply_delivery_report(SignedDeliveryReport::sign(
            report(
                &id,
                "svc-1",
                &provider,
                &wallet_id,
                NotificationTrigger::EscrowReleased,
                DeliveryStatus::Delivered,
            ),
            &provider,
        ));
        assert_eq!(result, Err(NotificationError::Unauthorized));
    }
}
