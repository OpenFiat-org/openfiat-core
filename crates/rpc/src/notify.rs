//! Where notifications actually come from: real protocol events reaching
//! this node.
//!
//! `openfiat-notifications` knows how to route and deliver, but it has no
//! opinion about *when*. This module is the wire — it sits on the same
//! gossip event stream every domain registry does, decides which
//! `NotificationTrigger` (if any) an event corresponds to and who cares
//! about it, plans the deliveries, and queues them for
//! `actor::poll_notifications` to hand to the gateways.
//!
//! Three properties hold by construction:
//!
//! - **It never breaks the protocol path.** The handler is installed
//!   *after* every domain registry's own `apply_event`, only reads
//!   already-applied state, and returns `()` unconditionally. A gateway
//!   being down, a subscription being malformed, a service having
//!   vanished — none of it can stop a settlement from being applied.
//! - **It does no I/O.** Planning is pure; the HTTP hop happens on the
//!   actor's own tick, so nothing here can stall gossip.
//! - **It is deterministic.** Every node runs this same mapping over the
//!   same replicated state and derives the same
//!   [`openfiat_notifications::NotificationId`]s, which is what lets the
//!   gateway collapse N nodes' copies into one delivery.
//!
//! Not every trigger in the taxonomy is wired: only the ones this node
//! genuinely observes today. `ReservationExpiring`, `TradeCompleted`,
//! `ReputationUpdated`, `VotingStarted`, `SnapshotAvailable`,
//! `NodeMaintenance`, and `ProviderOffline` have no event on this node's
//! wire that honestly corresponds to them, and are deliberately left
//! unmapped rather than faked.

use openfiat_advertisements::AdvertisementRegistry;
use openfiat_disputes::{DisputeRegistry, DisputeStatus};
use openfiat_notifications::routing::PlannedDelivery;
use openfiat_notifications::{NotificationRegistry, NotificationTrigger};
use openfiat_serialization::wire;
use openfiat_settlement::SettlementRegistry;
use openfiat_storage::KvStore;
use openfiat_types::{EventEnvelope, PeerId};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

/// Reads already-applied domain state to turn one observed event into
/// planned deliveries.
pub struct NotificationDispatcher<S> {
    notifications: Rc<NotificationRegistry<S>>,
    advertisements: Rc<AdvertisementRegistry<S>>,
    settlements: Rc<SettlementRegistry<S>>,
    disputes: Rc<DisputeRegistry<S>>,
    pending: Rc<RefCell<VecDeque<PlannedDelivery>>>,
}

impl<S: KvStore> NotificationDispatcher<S> {
    pub fn new(
        notifications: Rc<NotificationRegistry<S>>,
        advertisements: Rc<AdvertisementRegistry<S>>,
        settlements: Rc<SettlementRegistry<S>>,
        disputes: Rc<DisputeRegistry<S>>,
        pending: Rc<RefCell<VecDeque<PlannedDelivery>>>,
    ) -> Self {
        Self {
            notifications,
            advertisements,
            settlements,
            disputes,
            pending,
        }
    }

    /// Map one gossip event onto notifications, if it maps onto any.
    ///
    /// Must be installed after the domain registries' own handlers: this
    /// reads the state they just wrote (a settlement's buyer and seller,
    /// a dispute's parties) rather than re-deriving it from the payload.
    pub fn observe(&self, event: &EventEnvelope) {
        let source = *event.id.as_bytes();
        match (event.ofs_spec, event.event_type.as_str()) {
            (openfiat_advertisements::protocol::OFS_SPEC, name)
                if name == openfiat_advertisements::protocol::EVENT_DISABLED =>
            {
                let Ok(signed) = wire::from_bytes::<
                    openfiat_advertisements::events::SignedAdvertisementDisable,
                >(&event.payload) else {
                    return;
                };
                self.notify(
                    NotificationTrigger::AdvertisementDisabled,
                    &source,
                    &[signed.disable.merchant],
                );
            }
            (openfiat_reservations::protocol::OFS_SPEC, name)
                if name == openfiat_reservations::protocol::EVENT_REQUESTED =>
            {
                let Ok(signed) = wire::from_bytes::<
                    openfiat_reservations::events::SignedReservationRequest,
                >(&event.payload) else {
                    return;
                };
                // Both sides of the trade care: the taker that it was
                // accepted, the merchant that they have work to do.
                let mut recipients = vec![signed.request.requester];
                if let Some(advertisement) =
                    self.advertisements.get(&signed.request.advertisement_id)
                {
                    recipients.push(advertisement.merchant);
                }
                self.notify(
                    NotificationTrigger::ReservationCreated,
                    &source,
                    &recipients,
                );
            }
            (openfiat_settlement::protocol::OFS_SPEC, name)
                if name == openfiat_settlement::protocol::EVENT_PAYMENT_SUBMITTED =>
            {
                let Ok(signed) = wire::from_bytes::<
                    openfiat_settlement::events::SignedPaymentSubmitted,
                >(&event.payload) else {
                    return;
                };
                // Only the seller: the buyer is the one who just declared it.
                let Some(settlement) = self.settlements.get(&signed.action.settlement_id) else {
                    return;
                };
                self.notify(
                    NotificationTrigger::PaymentSubmitted,
                    &source,
                    &[settlement.seller],
                );
            }
            (openfiat_settlement::protocol::OFS_SPEC, name)
                if name == openfiat_settlement::protocol::EVENT_APPROVED =>
            {
                let Ok(signed) = wire::from_bytes::<
                    openfiat_settlement::events::SignedSettlementApproved,
                >(&event.payload) else {
                    return;
                };
                let Some(settlement) = self.settlements.get(&signed.action.settlement_id) else {
                    return;
                };
                self.notify(
                    NotificationTrigger::SettlementApproved,
                    &source,
                    &[settlement.buyer, settlement.seller],
                );
            }
            (openfiat_disputes::protocol::OFS_SPEC, name)
                if name == openfiat_disputes::protocol::EVENT_OPENED =>
            {
                let Ok(signed) = wire::from_bytes::<openfiat_disputes::events::SignedDisputeOpen>(
                    &event.payload,
                ) else {
                    return;
                };
                // OFS-2400 opens the evidence window at the moment a
                // dispute is opened, so `DisputeOpened` is what
                // `EvidenceRequested` corresponds to on this wire —
                // there is no separate evidence-request event.
                let Some(dispute) = self.disputes.get(&signed.open.id) else {
                    return;
                };
                self.notify(
                    NotificationTrigger::EvidenceRequested,
                    &source,
                    &[dispute.buyer, dispute.seller],
                );
            }
            (openfiat_disputes::protocol::OFS_SPEC, name)
                if name == openfiat_disputes::protocol::EVENT_VOTE_REVEALED =>
            {
                let Ok(signed) =
                    wire::from_bytes::<openfiat_disputes::events::SignedVoteReveal>(&event.payload)
                else {
                    return;
                };
                // Resolution is a local derivation from the reveals, not
                // its own event (see `openfiat_disputes::protocol`), so
                // the notification fires on the reveal that completes it.
                // Every node derives resolution identically, so every
                // node fires on the same reveal.
                let Some(dispute) = self.disputes.get(&signed.reveal.dispute_id) else {
                    return;
                };
                if dispute.status != DisputeStatus::Resolved {
                    return;
                }
                self.notify(
                    NotificationTrigger::ResolutionIssued,
                    &source,
                    &[dispute.buyer, dispute.seller],
                );
            }
            (openfiat_governance::protocol::OFS_SPEC, name)
                if name == openfiat_governance::protocol::EVENT_CREATED =>
            {
                self.broadcast(NotificationTrigger::ProposalPublished, &source);
            }
            (openfiat_governance::protocol::OFS_SPEC, name)
                if name == openfiat_governance::protocol::EVENT_ACTIVATED =>
            {
                self.broadcast(NotificationTrigger::ProposalActivated, &source);
            }
            _ => {}
        }
    }

    /// `EscrowReleased` has no gossip event of its own — the node learns
    /// it by watching a relayed `release_escrow` transaction confirm on
    /// Solana (OFS-4300). The confirmed transaction signature stands in
    /// for an `EventId` as the source identity: it is equally canonical
    /// and equally agreed on by every node that observes the same
    /// confirmation.
    ///
    /// Only `RpcConnected` nodes see this, so the dedup population is
    /// smaller than for a gossiped trigger. That is harmless — dedup is
    /// the gateway's job either way, and fewer witnesses never produces a
    /// duplicate, only fewer copies of the same id.
    pub fn observe_escrow_release(
        &self,
        settlement_id: &openfiat_settlement::SettlementId,
        signature: &str,
    ) {
        let Some(settlement) = self.settlements.get(settlement_id) else {
            return;
        };
        self.notify(
            NotificationTrigger::EscrowReleased,
            signature.as_bytes(),
            &[settlement.buyer, settlement.seller],
        );
    }

    /// Governance reaches everyone who opted into the category, not a
    /// specific counterparty — derived from replicated subscriptions so
    /// the recipient set is the same on every node.
    fn broadcast(&self, trigger: NotificationTrigger, source: &[u8]) {
        let recipients = openfiat_notifications::routing::broadcast_recipients(
            &self.notifications.all_subscriptions(),
            trigger,
        );
        self.notify(trigger, source, &recipients);
    }

    fn notify(&self, trigger: NotificationTrigger, source: &[u8], recipients: &[PeerId]) {
        for recipient in recipients {
            let plan = self.notifications.plan(trigger, source, recipient);
            for skipped in &plan.skipped {
                // Visible, not silent: a wallet whose gateway quietly
                // deregistered would otherwise just stop receiving
                // notifications with nothing anywhere explaining why.
                eprintln!(
                    "openfiat-notifications: skipping {} for service {} ({})",
                    trigger.name(),
                    skipped.service_id.as_str(),
                    skipped.reason.as_str()
                );
            }
            for delivery in plan.deliveries {
                self.notifications.record_queued(&delivery);
                self.pending.borrow_mut().push_back(delivery);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::NodeState;
    use openfiat_crypto::{Keypair, seal};
    use openfiat_network::identity::peer_id_from_public_key;
    use openfiat_notifications::events::{SignedSubscriptionUpdate, SubscriptionUpdate};
    use openfiat_notifications::{NotificationCategory, NotificationId, SubscriptionDestination};
    use openfiat_registry::{Registration, SignedRegistration};
    use openfiat_settlement::events::{
        PaymentSubmitted, SettlementApproved, SettlementInitiate, SignedPaymentSubmitted,
        SignedSettlementApproved, SignedSettlementInitiate,
    };
    use openfiat_storage::mem::MemoryStore;
    use openfiat_types::{
        Amount, EventType, NotificationChannel, Priority, ServiceId, ServiceType, Timestamp,
    };

    const GATEWAY_ENDPOINT: &str = "https://gw.example/deliver";

    fn register_gateway(state: &NodeState<MemoryStore>, gateway: &Keypair) {
        state
            .services
            .apply_registration(SignedRegistration::sign(
                Registration {
                    service_id: ServiceId::new("gw-1"),
                    service_type: ServiceType::Notifications(NotificationChannel::Email),
                    provider: peer_id_from_public_key(&gateway.public_key()).unwrap(),
                    provider_public_key: gateway.public_key(),
                    endpoints: vec![GATEWAY_ENDPOINT.to_string()],
                    supported_ofs: vec![6000],
                    region: None,
                    capabilities: vec![],
                    pricing: None,
                    payout_wallet: None,
                    timestamp: Timestamp::now(),
                },
                gateway,
            ))
            .unwrap();
    }

    fn subscribe(
        state: &NodeState<MemoryStore>,
        wallet: &Keypair,
        gateway: &Keypair,
        categories: Vec<NotificationCategory>,
    ) {
        let update = SubscriptionUpdate {
            wallet: peer_id_from_public_key(&wallet.public_key()).unwrap(),
            wallet_public_key: wallet.public_key(),
            enabled_categories: categories,
            destinations: vec![SubscriptionDestination {
                service_id: ServiceId::new("gw-1"),
                channel: NotificationChannel::Email,
                sealed: seal(&gateway.public_key(), b"user@example.com").unwrap(),
            }],
            timestamp: Timestamp::now(),
        };
        state
            .notifications
            .apply_subscription_update(SignedSubscriptionUpdate::sign(update, wallet))
            .unwrap();
    }

    /// Puts a real settlement through Initiate -> PaymentSubmitted, both
    /// as genuine signed events originated on this node's own gossip
    /// service, so every handler runs exactly as it would in production.
    fn originate_settlement_to_payment_submitted(
        state: &NodeState<MemoryStore>,
        buyer: &Keypair,
        seller: &Keypair,
    ) -> openfiat_settlement::SettlementId {
        let settlement_id = openfiat_settlement::SettlementId::new("stl-1");
        let initiate = SignedSettlementInitiate::sign(
            SettlementInitiate {
                id: settlement_id.clone(),
                reservation_id: openfiat_reservations::ReservationId::new("rsv-1"),
                buyer: peer_id_from_public_key(&buyer.public_key()).unwrap(),
                buyer_public_key: buyer.public_key(),
                seller: peer_id_from_public_key(&seller.public_key()).unwrap(),
                seller_public_key: seller.public_key(),
                amount: Amount::new(1_000_000, 6),
                timestamp: Timestamp::now(),
            },
            buyer,
        );
        originate(
            state,
            openfiat_settlement::protocol::EVENT_INITIATED,
            openfiat_settlement::protocol::OFS_SPEC,
            &initiate,
        );

        let payment = SignedPaymentSubmitted::sign(
            PaymentSubmitted {
                settlement_id: settlement_id.clone(),
                buyer: peer_id_from_public_key(&buyer.public_key()).unwrap(),
                payment_reference: None,
                timestamp: Timestamp::now(),
            },
            buyer,
        );
        originate(
            state,
            openfiat_settlement::protocol::EVENT_PAYMENT_SUBMITTED,
            openfiat_settlement::protocol::OFS_SPEC,
            &payment,
        );

        settlement_id
    }

    fn originate(
        state: &NodeState<MemoryStore>,
        event_type: &str,
        ofs_spec: u16,
        payload: &impl serde::Serialize,
    ) -> openfiat_types::EventId {
        state
            .gossip
            .borrow_mut()
            .originate(
                EventType::new(event_type).unwrap(),
                ofs_spec,
                Priority::SessionReservationSettlement,
                4,
                wire::to_bytes(payload).unwrap(),
            )
            .expect("this node may originate settlement events")
    }

    /// The wiring test: a real signed settlement approval, originated on
    /// a real gossip service, must come out the other end as queued
    /// deliveries. Nothing here calls the dispatcher directly.
    #[test]
    fn approving_a_settlement_queues_a_delivery_for_both_parties() {
        let state = NodeState::new_for_test(MemoryStore::new());
        let gateway = Keypair::generate();
        let buyer = Keypair::generate();
        let seller = Keypair::generate();
        register_gateway(&state, &gateway);
        subscribe(
            &state,
            &buyer,
            &gateway,
            vec![NotificationCategory::Trading],
        );
        subscribe(
            &state,
            &seller,
            &gateway,
            vec![NotificationCategory::Trading],
        );

        let settlement_id = originate_settlement_to_payment_submitted(&state, &buyer, &seller);
        // Drop the PaymentSubmitted notification the step above produced.
        let payment_notifications = state.drain_notifications();
        assert_eq!(
            payment_notifications.len(),
            1,
            "PaymentSubmitted goes to the seller only — the buyer is who declared it"
        );
        assert_eq!(
            payment_notifications[0].payload.recipient_wallet,
            peer_id_from_public_key(&seller.public_key()).unwrap()
        );

        let approval = SignedSettlementApproved::sign(
            SettlementApproved {
                settlement_id: settlement_id.clone(),
                seller: peer_id_from_public_key(&seller.public_key()).unwrap(),
                timestamp: Timestamp::now(),
            },
            &seller,
        );
        let event_id = originate(
            &state,
            openfiat_settlement::protocol::EVENT_APPROVED,
            openfiat_settlement::protocol::OFS_SPEC,
            &approval,
        );

        let queued = state.drain_notifications();
        assert_eq!(queued.len(), 2, "both counterparties are notified");
        for delivery in &queued {
            assert_eq!(delivery.endpoint, GATEWAY_ENDPOINT);
            assert_eq!(
                delivery.payload.notification_id,
                NotificationId::derive(
                    NotificationTrigger::SettlementApproved,
                    event_id.as_bytes(),
                    &delivery.payload.recipient_wallet
                ),
                "the id must be derivable by any other node from the same event"
            );
            // The node queued it without ever being able to read it.
            assert_eq!(
                openfiat_crypto::open(&gateway, &delivery.payload.sealed_destination).unwrap(),
                b"user@example.com"
            );
            assert_eq!(
                state
                    .notifications
                    .dispatch(&delivery.payload.notification_id)
                    .unwrap()
                    .status,
                openfiat_notifications::DeliveryStatus::Queued
            );
        }
    }

    /// The settlement itself must land whether or not anybody can be
    /// notified about it — no gateway, no subscription, no problem.
    #[test]
    fn a_settlement_still_applies_when_nothing_can_be_delivered() {
        let state = NodeState::new_for_test(MemoryStore::new());
        let buyer = Keypair::generate();
        let seller = Keypair::generate();

        let settlement_id = originate_settlement_to_payment_submitted(&state, &buyer, &seller);
        let approval = SignedSettlementApproved::sign(
            SettlementApproved {
                settlement_id: settlement_id.clone(),
                seller: peer_id_from_public_key(&seller.public_key()).unwrap(),
                timestamp: Timestamp::now(),
            },
            &seller,
        );
        originate(
            &state,
            openfiat_settlement::protocol::EVENT_APPROVED,
            openfiat_settlement::protocol::OFS_SPEC,
            &approval,
        );

        assert_eq!(
            state.settlements.get(&settlement_id).unwrap().state,
            openfiat_settlement::SettlementState::Approved
        );
        assert!(state.drain_notifications().is_empty());
    }

    /// A wallet that opted into a different category gets nothing, and
    /// the absence is a routing decision rather than a delivery failure.
    #[test]
    fn a_wallet_subscribed_to_another_category_is_not_notified() {
        let state = NodeState::new_for_test(MemoryStore::new());
        let gateway = Keypair::generate();
        let buyer = Keypair::generate();
        let seller = Keypair::generate();
        register_gateway(&state, &gateway);
        subscribe(
            &state,
            &buyer,
            &gateway,
            vec![NotificationCategory::Governance],
        );

        let settlement_id = originate_settlement_to_payment_submitted(&state, &buyer, &seller);
        let approval = SignedSettlementApproved::sign(
            SettlementApproved {
                settlement_id,
                seller: peer_id_from_public_key(&seller.public_key()).unwrap(),
                timestamp: Timestamp::now(),
            },
            &seller,
        );
        originate(
            &state,
            openfiat_settlement::protocol::EVENT_APPROVED,
            openfiat_settlement::protocol::OFS_SPEC,
            &approval,
        );

        assert!(state.drain_notifications().is_empty());
    }

    /// Two nodes, two independent stores, one event — one id. This is the
    /// property the gateway's at-most-once guarantee rests on.
    #[test]
    fn two_nodes_observing_the_same_event_queue_the_same_notification_id() {
        let gateway = Keypair::generate();
        let buyer = Keypair::generate();
        let seller = Keypair::generate();

        let mut ids = Vec::new();
        let mut envelope = None;
        for round in 0..2 {
            let state = NodeState::new_for_test(MemoryStore::new());
            register_gateway(&state, &gateway);
            subscribe(
                &state,
                &buyer,
                &gateway,
                vec![NotificationCategory::Trading],
            );
            let settlement_id = originate_settlement_to_payment_submitted(&state, &buyer, &seller);
            state.drain_notifications();

            if round == 0 {
                // Node A originates the approval; node B then observes
                // that same envelope, exactly as it would off the wire.
                let captured: Rc<RefCell<Option<EventEnvelope>>> = Rc::new(RefCell::new(None));
                let captured_for_handler = Rc::clone(&captured);
                state.gossip.borrow_mut().add_event_handler(move |event| {
                    if event.event_type.as_str() == openfiat_settlement::protocol::EVENT_APPROVED {
                        *captured_for_handler.borrow_mut() = Some(event.clone());
                    }
                });
                let approval = SignedSettlementApproved::sign(
                    SettlementApproved {
                        settlement_id,
                        seller: peer_id_from_public_key(&seller.public_key()).unwrap(),
                        timestamp: Timestamp::now(),
                    },
                    &seller,
                );
                originate(
                    &state,
                    openfiat_settlement::protocol::EVENT_APPROVED,
                    openfiat_settlement::protocol::OFS_SPEC,
                    &approval,
                );
                envelope = captured.borrow_mut().take();
            } else {
                state
                    .notification_dispatcher
                    .observe(envelope.as_ref().unwrap());
            }

            let queued = state.drain_notifications();
            assert_eq!(queued.len(), 1);
            ids.push(queued[0].payload.notification_id.clone());
        }

        assert_eq!(
            ids[0], ids[1],
            "two independent nodes must mint the same id, or the user gets one message per node"
        );
    }
}
