//! Wire-level constants: gossip event names (drawn from OFS-8100's ADV
//! namespace) and the OFS spec number this crate's events belong to.

pub const OFS_SPEC: u16 = 2100;

pub const EVENT_CREATED: &str = "AdvertisementCreated";
/// Was `AdvertisementDisabled`, when a status could only go one way.
/// Renamed rather than kept alongside a new name: two event types that
/// both set a status is a rule with two places to be wrong.
pub const EVENT_STATUS_SET: &str = "AdvertisementStatusSet";
pub const EVENT_TERMS_UPDATED: &str = "AdvertisementTermsUpdated";
pub const EVENT_PRICING_UPDATED: &str = "AdvertisementPricingUpdated";
