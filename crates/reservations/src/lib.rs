//! `openfiat-reservations` — Trade reservation and locking.
//!
//! Implements OFS-2200 (ORP) on top of `openfiat_gossip` and
//! `openfiat_advertisements`: reservation requests and cancellations
//! travel as gossip events, and processing one (whether self-originated
//! or received) deterministically validates it against the shared
//! advertisement registry and locks/releases liquidity accordingly
//! (§9-10, §15) — the same replication pattern used throughout this
//! workspace. This crate's authority ends at `EscrowLocked` (§5, §18);
//! everything after that belongs to `openfiat-settlement`.

pub mod error;
pub mod events;
pub mod protocol;
pub mod record;
pub mod service;
pub mod store;

pub use error::ReservationError;
pub use record::{Reservation, ReservationId, ReservationState};
pub use service::ReservationService;
pub use store::ReservationRegistry;

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
