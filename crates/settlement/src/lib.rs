//! `openfiat-settlement` — Settlement coordination with the Solana execution layer.
//!
//! Implements OFS-2300 (OSP) on top of `openfiat_gossip`: settlement
//! events (initiate/payment-submitted/payment-reversed/approved/rejected/
//! cancelled) travel as gossip events and every node derives its local
//! settlement state purely by consuming them — the same replication
//! pattern used throughout this workspace. This crate picks up authority
//! at `EscrowLocked` (§5, handed off from `openfiat-reservations`) and
//! its own authority ends where the on-chain program's escrow-release
//! instruction begins (see `record` module doc) — that Solana-side
//! integration is a separate, later piece of work.
//!
//! The [`recovery`] module is the exception to that boundary, and
//! deliberately so: a settlement that ends in a dispute the merchant's
//! vault could not fund leaves an obligation the settlement layer created
//! and the settlement layer should be able to see. It reads on-chain
//! state and computes what is still owed; like everything else here, it
//! signs nothing.

pub mod error;
pub mod events;
pub mod protocol;
pub mod record;
pub mod recovery;
pub mod service;
pub mod store;

pub use error::SettlementError;
pub use record::{PaymentDiscrepancy, Settlement, SettlementId, SettlementState};
pub use recovery::{MerchantStake, RecoveryClaim, RecoveryPlan};
pub use service::SettlementService;
pub use store::SettlementRegistry;

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
