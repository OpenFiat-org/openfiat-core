//! Drives one node's notification index: applies incoming gossip events
//! automatically and provides the operations that originate new ones.
//! Provider registration itself reuses `openfiat-registry` directly —
//! this crate has no registration event of its own, see the crate doc.

use crate::error::NotificationError;
use crate::events::{
    DeliveryReport, SignedDeliveryReport, SignedSubscriptionUpdate, SubscriptionUpdate,
};
use crate::protocol;
use crate::record::{
    DeliveryReceipt, DeliveryStatus, NotificationCategory, NotificationId, NotificationTrigger,
    Subscription,
};
use crate::store::NotificationRegistry;
use openfiat_gossip::GossipService;
use openfiat_registry::Registry;
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{EventType, PeerId, Priority, ServiceId, Timestamp};
use std::rc::Rc;

pub struct NotificationService<S> {
    pub gossip: GossipService<S>,
    registry: Rc<NotificationRegistry<S>>,
}

impl<S: KvStore + 'static> NotificationService<S> {
    /// `services` is the shared handle from `RegistryService::registry`
    /// on the same node — see `NotificationRegistry`.
    pub fn new(mut gossip: GossipService<S>, store: S, services: Rc<Registry<S>>) -> Self {
        let registry = Rc::new(NotificationRegistry::new(store, services));
        let handler_registry = Rc::clone(&registry);
        gossip.add_event_handler(move |event| handler_registry.apply_event(event));
        Self { gossip, registry }
    }

    pub fn subscription(&self, wallet: &PeerId) -> Option<Subscription> {
        self.registry.subscription(wallet)
    }

    pub fn receipt(&self, id: &NotificationId) -> Option<DeliveryReceipt> {
        self.registry.receipt(id)
    }

    pub fn receipts_for(&self, wallet: &PeerId) -> Vec<DeliveryReceipt> {
        self.registry.receipts_for(wallet)
    }

    /// §11: publish this node's own wallet subscription preferences.
    pub fn update_subscription(
        &mut self,
        enabled_categories: Vec<NotificationCategory>,
    ) -> Result<(), NotificationError> {
        let update = SubscriptionUpdate {
            wallet: self.gossip.node.local_peer_id(),
            wallet_public_key: self.gossip.public_key(),
            enabled_categories,
            timestamp: Timestamp::now(),
        };
        let bytes = wire::to_bytes(&update).map_err(|_| NotificationError::MalformedEvent)?;
        let signed = SignedSubscriptionUpdate {
            signature: self.gossip.sign(&bytes),
            update,
        };
        self.originate(protocol::EVENT_SUBSCRIPTION_UPDATED, &signed)
    }

    /// A provider (this node, registered via `openfiat-registry`) reports
    /// the outcome of one delivery attempt.
    pub fn report_delivery(
        &mut self,
        notification_id: NotificationId,
        service_id: ServiceId,
        recipient_wallet: PeerId,
        trigger: NotificationTrigger,
        status: DeliveryStatus,
    ) -> Result<(), NotificationError> {
        let report = DeliveryReport {
            notification_id,
            service_id,
            provider: self.gossip.node.local_peer_id(),
            provider_public_key: self.gossip.public_key(),
            recipient_wallet,
            trigger,
            status,
            timestamp: Timestamp::now(),
        };
        let bytes = wire::to_bytes(&report).map_err(|_| NotificationError::MalformedEvent)?;
        let signed = SignedDeliveryReport {
            signature: self.gossip.sign(&bytes),
            report,
        };
        self.originate(status.event_type_name(), &signed)
    }

    fn originate(
        &mut self,
        event_type: &str,
        payload: &impl serde::Serialize,
    ) -> Result<(), NotificationError> {
        let bytes = wire::to_bytes(payload).map_err(|_| NotificationError::MalformedEvent)?;
        let event_type = EventType::new(event_type)
            .expect("notification event names are all valid PascalCase identifiers");
        self.gossip
            .originate(
                event_type,
                protocol::OFS_SPEC,
                Priority::Reputation,
                8,
                bytes,
            )
            .map(|_| ())
            .map_err(|_| NotificationError::Unauthorized)
    }

    pub async fn drive_once(&mut self) {
        self.gossip.drive_once().await;
    }
}
