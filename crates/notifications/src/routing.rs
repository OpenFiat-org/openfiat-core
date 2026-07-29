//! Turning "this trigger fired for this wallet" into "POST this sealed
//! payload to these endpoints" — the selection step between a verified
//! protocol event and an actual delivery attempt.
//!
//! Routing is a pure function of already-replicated state: the wallet's
//! own signed subscription, plus `openfiat-registry`'s view of the
//! gateways it bound. Nothing here performs I/O, so every node reaches
//! the same conclusions from the same replica — which is what makes the
//! deterministic [`crate::NotificationId`] a workable deduplication key
//! rather than a coincidence.

use crate::provider::NotificationPayload;
use crate::record::{NotificationId, NotificationTrigger, Subscription};
use crate::render;
use openfiat_registry::Registry;
use openfiat_registry::record::HealthState;
use openfiat_storage::KvStore;
use openfiat_types::{PeerId, ServiceId, ServiceType};

/// One delivery this node intends to make: where to send it, and what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedDelivery {
    /// The gateway endpoint to POST to, taken from its `ServiceRecord`.
    pub endpoint: String,
    pub payload: NotificationPayload,
}

/// Why a bound destination produced no delivery.
///
/// Skipping is deliberately *reported*, not swallowed: a user whose
/// gateway silently deregistered would otherwise see notifications stop
/// with nothing anywhere saying why. Callers surface these (see
/// `openfiat_rpc::notify`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// The bound `service_id` is not in this node's registry replica —
    /// never registered, withdrawn, or expired as stale.
    GatewayUnknown,
    /// Registered, but not as a notification service at all.
    NotANotificationGateway,
    /// Registered as a notification gateway for a different channel than
    /// the subscription bound it for. Trusting the subscription alone
    /// would let a wallet address an SMS payload to an email gateway.
    ChannelMismatch,
    /// Registered and correct, but currently `Degraded` or `Offline`.
    Unhealthy,
    /// Registered with no endpoint to POST to.
    NoEndpoint,
}

impl SkipReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GatewayUnknown => "gateway-unknown",
            Self::NotANotificationGateway => "not-a-notification-gateway",
            Self::ChannelMismatch => "channel-mismatch",
            Self::Unhealthy => "gateway-unhealthy",
            Self::NoEndpoint => "gateway-has-no-endpoint",
        }
    }
}

/// A destination that was bound but not delivered to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedDestination {
    pub service_id: ServiceId,
    pub reason: SkipReason,
}

/// Everything routing decided for one (trigger, recipient) pair.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoutingPlan {
    pub deliveries: Vec<PlannedDelivery>,
    pub skipped: Vec<SkippedDestination>,
}

impl RoutingPlan {
    pub fn is_empty(&self) -> bool {
        self.deliveries.is_empty() && self.skipped.is_empty()
    }
}

/// `HealthState::Maintenance` is treated as deliverable: OFS-1500 §11
/// distinguishes a provider doing planned work from one that is failing,
/// and dropping notifications during a maintenance window would lose
/// them permanently rather than delay them.
const fn is_deliverable(health: HealthState) -> bool {
    matches!(health, HealthState::Online | HealthState::Maintenance)
}

/// Plan every delivery `trigger` should produce for `subscription`.
///
/// Returns an empty plan — not an error — when the wallet hasn't enabled
/// the trigger's category or has bound no destinations. Both are ordinary
/// user choices, and inventing a fallback destination for someone who
/// gave none would be worse than delivering nothing.
///
/// `source_event` is the causing event's identity; it flows straight into
/// [`NotificationId::derive`], so two nodes planning from the same event
/// mint the same ids.
pub fn plan<S: KvStore>(
    services: &Registry<S>,
    subscription: &Subscription,
    trigger: NotificationTrigger,
    source_event: &[u8],
) -> RoutingPlan {
    let mut plan = RoutingPlan::default();
    if !subscription.wants(trigger) {
        return plan;
    }

    let notification_id = NotificationId::derive(trigger, source_event, &subscription.wallet);
    let (subject, body) = render::compose(trigger);

    for destination in &subscription.destinations {
        let skip = |reason| SkippedDestination {
            service_id: destination.service_id.clone(),
            reason,
        };

        let Some(service) = services.get(&destination.service_id) else {
            plan.skipped.push(skip(SkipReason::GatewayUnknown));
            continue;
        };
        let ServiceType::Notifications(registered_channel) = service.service_type else {
            plan.skipped.push(skip(SkipReason::NotANotificationGateway));
            continue;
        };
        if registered_channel != destination.channel {
            plan.skipped.push(skip(SkipReason::ChannelMismatch));
            continue;
        }
        if !is_deliverable(service.health) {
            plan.skipped.push(skip(SkipReason::Unhealthy));
            continue;
        }
        let Some(endpoint) = service.endpoints.first() else {
            plan.skipped.push(skip(SkipReason::NoEndpoint));
            continue;
        };

        plan.deliveries.push(PlannedDelivery {
            endpoint: endpoint.clone(),
            payload: NotificationPayload {
                notification_id: notification_id.clone(),
                trigger,
                recipient_wallet: subscription.wallet.clone(),
                service_id: destination.service_id.clone(),
                channel: destination.channel,
                sealed_destination: destination.sealed.clone(),
                subject: subject.clone(),
                body: body.clone(),
            },
        });
    }

    plan
}

/// The wallets a broadcast-style trigger (a governance proposal, say)
/// should reach: everyone who opted into its category. Deliberately
/// derived from replicated subscription state rather than a peer list,
/// so it is the same set on every node.
pub fn broadcast_recipients(
    subscriptions: &[Subscription],
    trigger: NotificationTrigger,
) -> Vec<PeerId> {
    subscriptions
        .iter()
        .filter(|subscription| subscription.wants(trigger))
        .map(|subscription| subscription.wallet.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{NotificationCategory, SubscriptionDestination};
    use openfiat_crypto::{Keypair, seal};
    use openfiat_network::identity::peer_id_from_public_key;
    use openfiat_registry::{Registration, SignedRegistration};
    use openfiat_storage::mem::MemoryStore;
    use openfiat_types::{NotificationChannel, Timestamp};

    fn register(
        services: &Registry<MemoryStore>,
        gateway: &Keypair,
        service_id: &str,
        service_type: ServiceType,
        endpoints: Vec<String>,
    ) {
        let registration = Registration {
            service_id: ServiceId::new(service_id),
            service_type,
            provider: peer_id_from_public_key(&gateway.public_key()).unwrap(),
            provider_public_key: gateway.public_key(),
            endpoints,
            supported_ofs: vec![6000],
            region: None,
            capabilities: vec![],
            pricing: None,
            payout_wallet: None,
            timestamp: Timestamp::now(),
        };
        services
            .apply_registration(SignedRegistration::sign(registration, gateway))
            .unwrap();
    }

    fn subscription(wallet: &Keypair, destinations: Vec<SubscriptionDestination>) -> Subscription {
        Subscription {
            wallet: peer_id_from_public_key(&wallet.public_key()).unwrap(),
            wallet_public_key: wallet.public_key(),
            enabled_categories: vec![NotificationCategory::Trading],
            destinations,
            updated_at: Timestamp::now(),
        }
    }

    fn destination(gateway: &Keypair, service_id: &str) -> SubscriptionDestination {
        SubscriptionDestination {
            service_id: ServiceId::new(service_id),
            channel: NotificationChannel::Email,
            sealed: seal(&gateway.public_key(), b"user@example.com").unwrap(),
        }
    }

    #[test]
    fn routes_to_a_live_gateway_bound_for_the_right_channel() {
        let services = Registry::new(MemoryStore::new());
        let gateway = Keypair::generate();
        register(
            &services,
            &gateway,
            "gw-1",
            ServiceType::Notifications(NotificationChannel::Email),
            vec!["https://gw.example/deliver".to_string()],
        );
        let wallet = Keypair::generate();
        let subscription = subscription(&wallet, vec![destination(&gateway, "gw-1")]);

        let plan = plan(
            &services,
            &subscription,
            NotificationTrigger::SettlementApproved,
            b"event-1",
        );

        assert_eq!(plan.skipped, vec![]);
        assert_eq!(plan.deliveries.len(), 1);
        assert_eq!(plan.deliveries[0].endpoint, "https://gw.example/deliver");
        assert_eq!(
            plan.deliveries[0].payload.notification_id,
            NotificationId::derive(
                NotificationTrigger::SettlementApproved,
                b"event-1",
                &subscription.wallet
            )
        );
    }

    /// The node must never be in a position to read the address it is
    /// forwarding — that is the entire privacy argument for sealing.
    #[test]
    fn the_planned_payload_carries_no_readable_destination() {
        let services = Registry::new(MemoryStore::new());
        let gateway = Keypair::generate();
        register(
            &services,
            &gateway,
            "gw-1",
            ServiceType::Notifications(NotificationChannel::Email),
            vec!["https://gw.example/deliver".to_string()],
        );
        let wallet = Keypair::generate();
        let subscription = subscription(&wallet, vec![destination(&gateway, "gw-1")]);

        let plan = plan(
            &services,
            &subscription,
            NotificationTrigger::SettlementApproved,
            b"event-1",
        );
        let sealed = &plan.deliveries[0].payload.sealed_destination;

        assert!(
            !sealed
                .ciphertext
                .windows(16)
                .any(|window| window == b"user@example.com")
        );
        assert_eq!(
            openfiat_crypto::open(&gateway, sealed).unwrap(),
            b"user@example.com",
            "and yet the bound gateway, and only it, can still read it"
        );
    }

    #[test]
    fn a_trigger_the_wallet_did_not_subscribe_to_produces_nothing() {
        let services = Registry::new(MemoryStore::new());
        let gateway = Keypair::generate();
        register(
            &services,
            &gateway,
            "gw-1",
            ServiceType::Notifications(NotificationChannel::Email),
            vec!["https://gw.example/deliver".to_string()],
        );
        let wallet = Keypair::generate();
        let subscription = subscription(&wallet, vec![destination(&gateway, "gw-1")]);

        // ProposalPublished is Governance; this wallet only enabled Trading.
        let plan = plan(
            &services,
            &subscription,
            NotificationTrigger::ProposalPublished,
            b"event-1",
        );
        assert!(plan.is_empty());
    }

    #[test]
    fn a_subscription_with_no_destinations_produces_nothing() {
        let services = Registry::new(MemoryStore::new());
        let wallet = Keypair::generate();
        let plan = plan(
            &services,
            &subscription(&wallet, vec![]),
            NotificationTrigger::SettlementApproved,
            b"event-1",
        );
        assert!(plan.is_empty());
    }

    #[test]
    fn an_unregistered_gateway_is_skipped_visibly() {
        let services = Registry::new(MemoryStore::new());
        let gateway = Keypair::generate();
        let wallet = Keypair::generate();
        let plan = plan(
            &services,
            &subscription(&wallet, vec![destination(&gateway, "gw-missing")]),
            NotificationTrigger::SettlementApproved,
            b"event-1",
        );
        assert!(plan.deliveries.is_empty());
        assert_eq!(plan.skipped[0].reason, SkipReason::GatewayUnknown);
        assert_eq!(plan.skipped[0].service_id, ServiceId::new("gw-missing"));
    }

    #[test]
    fn a_service_of_the_wrong_type_is_skipped_visibly() {
        let services = Registry::new(MemoryStore::new());
        let gateway = Keypair::generate();
        register(
            &services,
            &gateway,
            "gw-1",
            ServiceType::MarketData(openfiat_types::MarketDataService::PriceOracle),
            vec!["https://oracle.example".to_string()],
        );
        let wallet = Keypair::generate();
        let plan = plan(
            &services,
            &subscription(&wallet, vec![destination(&gateway, "gw-1")]),
            NotificationTrigger::SettlementApproved,
            b"event-1",
        );
        assert!(plan.deliveries.is_empty());
        assert_eq!(plan.skipped[0].reason, SkipReason::NotANotificationGateway);
    }

    #[test]
    fn a_gateway_registered_for_another_channel_is_skipped_visibly() {
        let services = Registry::new(MemoryStore::new());
        let gateway = Keypair::generate();
        register(
            &services,
            &gateway,
            "gw-1",
            ServiceType::Notifications(NotificationChannel::Sms),
            vec!["https://gw.example/deliver".to_string()],
        );
        let wallet = Keypair::generate();
        let plan = plan(
            &services,
            &subscription(&wallet, vec![destination(&gateway, "gw-1")]),
            NotificationTrigger::SettlementApproved,
            b"event-1",
        );
        assert!(plan.deliveries.is_empty());
        assert_eq!(plan.skipped[0].reason, SkipReason::ChannelMismatch);
    }

    #[test]
    fn a_gateway_with_no_endpoint_is_skipped_visibly() {
        let services = Registry::new(MemoryStore::new());
        let gateway = Keypair::generate();
        register(
            &services,
            &gateway,
            "gw-1",
            ServiceType::Notifications(NotificationChannel::Email),
            vec![],
        );
        let wallet = Keypair::generate();
        let plan = plan(
            &services,
            &subscription(&wallet, vec![destination(&gateway, "gw-1")]),
            NotificationTrigger::SettlementApproved,
            b"event-1",
        );
        assert!(plan.deliveries.is_empty());
        assert_eq!(plan.skipped[0].reason, SkipReason::NoEndpoint);
    }

    #[test]
    fn only_subscribers_to_the_category_are_broadcast_recipients() {
        let trading = Keypair::generate();
        let governance = Keypair::generate();
        let mut governance_subscription = subscription(&governance, vec![]);
        governance_subscription.enabled_categories = vec![NotificationCategory::Governance];

        let recipients = broadcast_recipients(
            &[subscription(&trading, vec![]), governance_subscription],
            NotificationTrigger::ProposalPublished,
        );

        assert_eq!(
            recipients,
            vec![peer_id_from_public_key(&governance.public_key()).unwrap()]
        );
    }
}
