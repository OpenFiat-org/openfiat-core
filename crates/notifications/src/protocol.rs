//! Wire-level constants. §14's delivery-status examples are a loose
//! "Examples" framing in ONP itself, not an exact list, so — per this
//! workspace's fallback convention — the delivery-receipt event names
//! come from OFS-8100 (OETR)'s canonical Notification Events vocabulary
//! instead (`DeliveryStatus::event_type_name`). `SubscriptionUpdated`
//! is also drawn from there; there's no separate `SubscriptionCreated`
//! event since a subscription is a single upserted-per-wallet record,
//! not a history of changes.

pub const OFS_SPEC: u16 = 6000;

pub const EVENT_SUBSCRIPTION_UPDATED: &str = "SubscriptionUpdated";

pub const DELIVERY_EVENT_NAMES: [&str; 7] =
    ["NotificationQueued", "NotificationSent", "NotificationDelivered", "NotificationRead", "NotificationFailed", "NotificationRetried", "NotificationExpired"];
