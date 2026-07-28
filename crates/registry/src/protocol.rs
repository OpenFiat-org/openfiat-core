//! Wire-level constants: the gossip event names this crate emits (drawn
//! from OFS-8100's canonical vocabulary, not invented locally) and the
//! numeric defaults proposed in `docs/architecture.md`.

use std::time::Duration;

pub const OFS_SPEC: u16 = 1500;

pub const EVENT_REGISTERED: &str = "ServiceRegistered";
pub const EVENT_UPDATED: &str = "ServiceUpdated";
pub const EVENT_UNREGISTERED: &str = "ServiceUnregistered";

/// §11: how often a provider should publish a health update.
pub const HEALTH_UPDATE_INTERVAL: Duration = Duration::from_secs(30);

/// §18: a service that hasn't published a health update within this long
/// auto-expires (three missed intervals).
pub const EXPIRATION_THRESHOLD: Duration = Duration::from_secs(90);
