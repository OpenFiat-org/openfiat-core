//! Canonical network priority classes.
//!
//! OFS-1000 §21, OFS-1200 §14, and OFS-1600 §10 each list priority tiers,
//! and the three lists are not byte-identical. OFS-1600 §10 ("Priority
//! Classes") is adopted here as canonical: it is the most granular of the
//! three and the one Stake-Weighted QoS actually schedules against. See
//! `docs/architecture.md` for the full reconciliation note.

/// Network message priority, per OFS-1600 §10. Lower ordinal = higher priority.
///
/// Stake-weighted ordering (SWQoS) applies *within* a class, never across
/// classes — a `SessionReservationSettlement` message from a low-stake node
/// still outranks a `TradeEscrow` message from a high-stake one.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[repr(u8)]
pub enum Priority {
    /// Session Control, Reservation, Settlement.
    SessionReservationSettlement = 1,
    /// Trade Updates, Escrow Events.
    TradeEscrow = 2,
    /// Advertisement Updates.
    Advertisement = 3,
    /// Reputation.
    Reputation = 4,
    /// Governance.
    Governance = 5,
    /// Snapshots.
    Snapshot = 6,
    /// Background Synchronization.
    BackgroundSync = 7,
}

impl Priority {
    /// All classes, highest priority first.
    pub const ALL: [Priority; 7] = [
        Priority::SessionReservationSettlement,
        Priority::TradeEscrow,
        Priority::Advertisement,
        Priority::Reputation,
        Priority::Governance,
        Priority::Snapshot,
        Priority::BackgroundSync,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_highest_priority_first() {
        assert!(Priority::SessionReservationSettlement < Priority::TradeEscrow);
        assert!(Priority::TradeEscrow < Priority::Advertisement);
        assert!(Priority::Snapshot < Priority::BackgroundSync);
    }
}
