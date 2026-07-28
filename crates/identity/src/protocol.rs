//! Wire-level constants. Event names follow OFS-5000 §17 exactly:
//! `ClaimCreated`, `ClaimVerified`, `ClaimRevoked`. `ClaimUpdated` (also in
//! §17) isn't a distinct event this crate emits — per §7/§11, an "update"
//! is just a fresh `ClaimCreated` for a new `ClaimId` with `supersedes` set.

pub const OFS_SPEC: u16 = 5000;

pub const EVENT_CREATED: &str = "ClaimCreated";
pub const EVENT_VERIFIED: &str = "ClaimVerified";
pub const EVENT_REVOKED: &str = "ClaimRevoked";
