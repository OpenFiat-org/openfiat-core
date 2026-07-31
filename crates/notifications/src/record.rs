//! Trigger taxonomy (OFS-6000 §10), wallet subscriptions (§11), and
//! delivery tracking (§14) — the replicated state this crate maintains
//! on top of `openfiat-registry`'s provider records.

use openfiat_crypto::{SealedBox, sha256};
use openfiat_types::{NotificationChannel, PeerId, PublicKey, ServiceId, Timestamp};

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
    /// The trigger's stable wire name. Used as the domain tag inside
    /// [`NotificationId::derive`], so renaming a variant here changes
    /// every id derived from it — that is intentional, an id is only
    /// meaningful relative to a fixed vocabulary.
    pub const fn name(self) -> &'static str {
        match self {
            Self::ReservationCreated => "ReservationCreated",
            Self::ReservationExpiring => "ReservationExpiring",
            Self::PaymentSubmitted => "PaymentSubmitted",
            Self::SettlementApproved => "SettlementApproved",
            Self::EscrowReleased => "EscrowReleased",
            Self::TradeCompleted => "TradeCompleted",
            Self::AdvertisementDisabled => "AdvertisementDisabled",
            Self::ReputationUpdated => "ReputationUpdated",
            Self::EvidenceRequested => "EvidenceRequested",
            Self::ResolutionIssued => "ResolutionIssued",
            Self::ProposalPublished => "ProposalPublished",
            Self::VotingStarted => "VotingStarted",
            Self::ProposalActivated => "ProposalActivated",
            Self::SnapshotAvailable => "SnapshotAvailable",
            Self::NodeMaintenance => "NodeMaintenance",
            Self::ProviderOffline => "ProviderOffline",
        }
    }

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
    /// Where deliveries actually go, each one sealed to the gateway that
    /// will perform it. `#[serde(default)]` so subscriptions gossiped
    /// before destinations existed still decode to an empty list instead
    /// of poisoning the store with undecodable rows.
    #[serde(default)]
    pub destinations: Vec<SubscriptionDestination>,
    pub updated_at: Timestamp,
}

impl Subscription {
    pub fn wants(&self, trigger: NotificationTrigger) -> bool {
        self.enabled_categories.contains(&trigger.category())
    }
}

/// One binding of "this wallet, on this channel, is reachable through
/// this gateway" — the unit routing selects (see [`crate::routing`]).
///
/// The destination itself is a [`SealedBox`] addressed to the bound
/// gateway's `provider_public_key`, never plaintext: a `SubscriptionUpdate`
/// is replicated to every node on the network, and §19 limits a provider
/// to "only what delivery requires". Nobody but that one gateway — not
/// the routing node, not any other node holding a replica — can read the
/// address.
///
/// A wallet with no destinations is legal and simply produces no
/// deliveries. There is deliberately no fallback destination: guessing
/// where to send someone's notifications is worse than sending none.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SubscriptionDestination {
    /// The `openfiat-registry` service this destination is sealed to and
    /// delivered through.
    pub service_id: ServiceId,
    /// Which channel the gateway is expected to deliver on. Routing
    /// checks this against the registered `ServiceType::Notifications`
    /// channel rather than trusting either side alone.
    pub channel: NotificationChannel,
    /// The delivery address, readable only by the bound gateway.
    pub sealed: SealedBox,
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

/// A notification's identity — and, critically, its **deduplication key**.
///
/// Gossip replicates every protocol event to every node, and every node
/// runs the same dispatcher over it. If ids were random or node-local, a
/// user with a subscription would receive one message *per node* that
/// observed the event: three copies on a three-node cluster, and a spam
/// cannon at any real scale.
///
/// [`NotificationId::derive`] closes that by making the id a pure
/// function of (trigger, source event, recipient). Every node
/// independently computes byte-identical ids for the same logical
/// notification, so the gateway sees N requests carrying one id and is
/// responsible for **at-most-once delivery**: deliver the first, ack the
/// rest as duplicates. The node cannot enforce that — it has no way to
/// know what its peers already handed over — so the guarantee lives with
/// the gateway by construction, and this id is the only thing that makes
/// it expressible.
///
/// The same property is what will later make notification metering
/// honest: N independent nodes reporting the same id is multi-witness
/// evidence that a handoff really happened, as opposed to today's
/// unverifiable gateway self-report. No metering is built on it yet.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct NotificationId(String);

impl NotificationId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The deterministic id for one (trigger, source event, recipient).
    ///
    /// `source_event` is the identity of whatever protocol event caused
    /// this notification — an `EventId`'s bytes for anything gossiped,
    /// or an equally-canonical substitute (a confirmed transaction
    /// signature, say) for something observed off the wire. It is taken
    /// as opaque bytes so callers are not forced to invent an `EventId`
    /// for events that genuinely do not have one.
    ///
    /// Every field is length-prefixed before hashing, so no two distinct
    /// inputs can concatenate to the same transcript.
    pub fn derive(
        trigger: NotificationTrigger,
        source_event: &[u8],
        recipient_wallet: &PeerId,
    ) -> Self {
        let mut transcript = Vec::new();
        for field in [
            b"openfiat/notification-id/v1".as_slice(),
            trigger.name().as_bytes(),
            source_event,
            recipient_wallet.as_bytes(),
        ] {
            transcript.extend_from_slice(&(field.len() as u32).to_le_bytes());
            transcript.extend_from_slice(field);
        }
        let digest = sha256(&transcript);
        let mut hex = String::with_capacity(64);
        for byte in digest {
            use std::fmt::Write as _;
            let _ = write!(hex, "{byte:02x}");
        }
        Self(hex)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What *this node* observed about one notification it routed itself —
/// kept strictly apart from [`DeliveryReceipt`], which is what a gateway
/// *claims* happened afterwards.
///
/// The split matters. A node can honestly witness exactly one thing: that
/// it handed a sealed payload to a gateway endpoint and got a 2xx back
/// (`Sent`), or did not (`Failed`). It cannot see whether the email
/// bounced or the push was opened; only the gateway can, and it reports
/// that separately. Storing the two in one place would blur a fact the
/// node checked with a claim it merely received.
///
/// This record is also what makes a gateway report *checkable*: a report
/// is only accepted for an id this node actually dispatched, to the
/// service it was actually routed to. See
/// `NotificationRegistry::apply_delivery_report`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DispatchRecord {
    pub notification_id: NotificationId,
    pub service_id: ServiceId,
    pub recipient_wallet: PeerId,
    pub trigger: NotificationTrigger,
    pub channel: NotificationChannel,
    /// `Queued` once planned, then `Sent` or `Failed` once the handoff
    /// to the gateway has actually been attempted. Never advances past
    /// the handoff — anything beyond that is the gateway's to report.
    pub status: DeliveryStatus,
    pub updated_at: Timestamp,
}

/// §14/§18: a provider's report of what happened to one delivery
/// attempt — feeds a provider's operational reputation (§18) the same
/// way `openfiat-reputation` derives a wallet's from marketplace events.
/// Keyed by `notification_id`, latest report wins (no history retained).
///
/// This is the **last-mile** half of delivery state, and it is
/// necessarily gateway-signed: `Delivered`, `Read`, `Retried`, `Expired`
/// and a bounced `Failed` are outcomes no node can observe. The node's
/// own half — did the handoff to the gateway succeed at all — lives in
/// [`DispatchRecord`], and is what constrains which reports are even
/// accepted here.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DeliveryReceipt {
    pub notification_id: NotificationId,
    pub service_id: ServiceId,
    pub recipient_wallet: PeerId,
    pub trigger: NotificationTrigger,
    pub status: DeliveryStatus,
    pub updated_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wallet() -> PeerId {
        PeerId::from_bytes(b"wallet-alpha".to_vec())
    }

    /// The whole point of directive #3: two nodes, no shared state, same
    /// answer — otherwise every subscriber gets one message per node.
    #[test]
    fn two_independent_nodes_derive_byte_identical_ids() {
        let node_a = NotificationId::derive(
            NotificationTrigger::SettlementApproved,
            b"event-id-bytes",
            &wallet(),
        );
        let node_b = NotificationId::derive(
            NotificationTrigger::SettlementApproved,
            b"event-id-bytes",
            &wallet(),
        );
        assert_eq!(node_a, node_b);
        assert_eq!(node_a.as_str().len(), 64, "hex-encoded SHA-256");
    }

    #[test]
    fn a_different_trigger_derives_a_different_id() {
        assert_ne!(
            NotificationId::derive(NotificationTrigger::SettlementApproved, b"e", &wallet()),
            NotificationId::derive(NotificationTrigger::EscrowReleased, b"e", &wallet()),
        );
    }

    #[test]
    fn a_different_source_event_derives_a_different_id() {
        assert_ne!(
            NotificationId::derive(NotificationTrigger::SettlementApproved, b"e1", &wallet()),
            NotificationId::derive(NotificationTrigger::SettlementApproved, b"e2", &wallet()),
        );
    }

    /// One event notifying two parties must not collapse into one id —
    /// the gateway would dedup the second recipient away entirely.
    #[test]
    fn a_different_recipient_derives_a_different_id() {
        assert_ne!(
            NotificationId::derive(NotificationTrigger::SettlementApproved, b"e", &wallet()),
            NotificationId::derive(
                NotificationTrigger::SettlementApproved,
                b"e",
                &PeerId::from_bytes(b"wallet-beta".to_vec()),
            ),
        );
    }

    /// Length prefixing is what stops ("ab", "c") and ("a", "bc") from
    /// hashing to the same transcript.
    #[test]
    fn adjacent_fields_cannot_be_slid_into_each_other() {
        assert_ne!(
            NotificationId::derive(
                NotificationTrigger::SettlementApproved,
                b"ab",
                &PeerId::from_bytes(b"c".to_vec()),
            ),
            NotificationId::derive(
                NotificationTrigger::SettlementApproved,
                b"a",
                &PeerId::from_bytes(b"bc".to_vec()),
            ),
        );
    }

    /// Already-gossiped subscriptions predate `destinations` entirely; if
    /// they stopped decoding, every one of them would vanish from the
    /// store on the next restart.
    ///
    /// The identifiers here are base58, which is how `PeerId` and
    /// `PublicKey` render in JSON. That is not what makes this fixture
    /// "legacy" — the absent `destinations` field is. Stored state is
    /// replayed from postcard-encoded gossip events rather than from JSON
    /// rows, so this shape is a hand-built stand-in for a decode, not a
    /// row anyone's disk actually holds.
    #[test]
    fn a_subscription_without_destinations_still_decodes() {
        let legacy = serde_json::json!({
            "wallet": bs58::encode(b"wallet-alpha").into_string(),
            "wallet_public_key": bs58::encode([0u8; 32]).into_string(),
            "enabled_categories": ["Trading"],
            "updated_at": Timestamp::now(),
        });
        let decoded: Subscription = serde_json::from_value(legacy).unwrap();
        assert!(decoded.destinations.is_empty());
        assert!(decoded.wants(NotificationTrigger::TradeCompleted));
    }
}
