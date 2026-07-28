//! Trigger taxonomy (OFS-6000 §10), wallet subscriptions (§11), and
//! delivery tracking (§14) — the replicated state this crate maintains
//! on top of `openfiat-registry`'s provider records.

use openfiat_types::{PeerId, PublicKey, ServiceId, Timestamp};

/// §10's grouping — the granularity a wallet actually opts in/out at
/// (§11's example: "Trade Notifications ✓, Governance ✓, Marketing ✗"),
/// coarser than the individual triggers below.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum NotificationCategory {
    Trading,
    Marketplace,
    Disputes,
    Governance,
    Infrastructure,
}

/// §10's examples, grouped under the category a subscription is
/// expressed against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum NotificationTrigger {
    ReservationCreated,
    ReservationExpiring,
    PaymentSubmitted,
    SettlementApproved,
    EscrowReleased,
    TradeCompleted,
    AdvertisementDisabled,
    ReputationUpdated,
    EvidenceRequested,
    ResolutionIssued,
    ProposalPublished,
    VotingStarted,
    ProposalActivated,
    SnapshotAvailable,
    NodeMaintenance,
    ProviderOffline,
}

impl NotificationTrigger {
    pub const fn category(self) -> NotificationCategory {
        match self {
            Self::ReservationCreated
            | Self::ReservationExpiring
            | Self::PaymentSubmitted
            | Self::SettlementApproved
            | Self::EscrowReleased
            | Self::TradeCompleted => NotificationCategory::Trading,
            Self::AdvertisementDisabled | Self::ReputationUpdated => {
                NotificationCategory::Marketplace
            }
            Self::EvidenceRequested | Self::ResolutionIssued => NotificationCategory::Disputes,
            Self::ProposalPublished | Self::VotingStarted | Self::ProposalActivated => {
                NotificationCategory::Governance
            }
            Self::SnapshotAvailable | Self::NodeMaintenance | Self::ProviderOffline => {
                NotificationCategory::Infrastructure
            }
        }
    }
}

/// §11: "Subscriptions belong to the wallet and synchronize across
/// compatible applications" — replicated the same way every other
/// wallet-portable record in this workspace is, keyed by wallet with
/// upsert semantics (the latest `SubscriptionUpdate` fully replaces the
/// previous one, no history retained).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Subscription {
    pub wallet: PeerId,
    pub wallet_public_key: PublicKey,
    pub enabled_categories: Vec<NotificationCategory>,
    pub updated_at: Timestamp,
}

impl Subscription {
    pub fn wants(&self, trigger: NotificationTrigger) -> bool {
        self.enabled_categories.contains(&trigger.category())
    }
}

/// §14's examples are a loose "Examples" framing in ONP itself, so this
/// crate follows this workspace's fallback convention and uses OFS-8100
/// (OETR)'s canonical Notification Events vocabulary instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DeliveryStatus {
    Queued,
    Sent,
    Delivered,
    Read,
    Failed,
    Retried,
    Expired,
}

impl DeliveryStatus {
    pub const fn event_type_name(self) -> &'static str {
        match self {
            Self::Queued => "NotificationQueued",
            Self::Sent => "NotificationSent",
            Self::Delivered => "NotificationDelivered",
            Self::Read => "NotificationRead",
            Self::Failed => "NotificationFailed",
            Self::Retried => "NotificationRetried",
            Self::Expired => "NotificationExpired",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct NotificationId(String);

impl NotificationId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// §14/§18: a provider's report of what happened to one delivery
/// attempt — feeds a provider's operational reputation (§18) the same
/// way `openfiat-reputation` derives a wallet's from marketplace events.
/// Keyed by `notification_id`, latest report wins (no history retained).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DeliveryReceipt {
    pub notification_id: NotificationId,
    pub service_id: ServiceId,
    pub recipient_wallet: PeerId,
    pub trigger: NotificationTrigger,
    pub status: DeliveryStatus,
    pub updated_at: Timestamp,
}
