//! `openfiat-snapshot` — State snapshot creation and synchronization between nodes.
//!
//! Implements OFS-1300 (SSP) on top of `openfiat_gossip` and
//! `openfiat_registry`: providers register exactly the way any other
//! service does (`ServiceType::Infrastructure(InfrastructureService::SnapshotProvider)`,
//! per decision #9 — no separate registration event here, and no
//! standalone spec of its own), signed snapshot announcements travel as
//! gossip events (only metadata — §12), and every node derives its known
//! snapshot set purely by consuming them. `codec` and `store::SnapshotIndex::import`
//! implement §16-17's verify/decompress/state-root pipeline against
//! whatever bytes the caller downloaded through its own transport (§14
//! is explicitly out of this crate's scope). `record`'s doc explains why
//! this crate never looks inside a snapshot's state bytes.

pub mod codec;
pub mod error;
pub mod events;
pub mod protocol;
pub mod record;
pub mod service;
pub mod store;

pub use error::SnapshotError;
pub use record::{CompressionMethod, SnapshotId, SnapshotMetadata};
pub use service::SnapshotService;
pub use store::SnapshotIndex;

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
