//! `openfiat-identity` — Identity claims and verification.
//!
//! Implements OFS-5000 (OICP) on top of `openfiat_gossip`: claim
//! lifecycle events (published/verified/revoked) travel as gossip
//! events, and every node derives its local claim index purely by
//! consuming them, the same replication pattern used throughout this
//! workspace. Real OTP delivery/verification is deferred — see the
//! `record` module doc.

pub mod error;
pub mod events;
pub mod protocol;
pub mod record;
pub mod service;
pub mod store;

pub use error::IdentityError;
pub use record::{Claim, ClaimId, ClaimType, VerificationStatus};
pub use service::IdentityService;
pub use store::IdentityRegistry;

/// Crate version, re-exported for diagnostics and `openfiat-node --version`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_a_version() {
        assert!(!version().is_empty());
    }
}
