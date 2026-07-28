//! Wire-level constants: gossip event names (drawn from OFS-8100's ADV
//! namespace) and the OFS spec number this crate's events belong to.

pub const OFS_SPEC: u16 = 2100;

pub const EVENT_CREATED: &str = "AdvertisementCreated";
pub const EVENT_DISABLED: &str = "AdvertisementDisabled";
pub const EVENT_PRICING_UPDATED: &str = "AdvertisementPricingUpdated";
