//! `openfiat-snapshot` — State snapshot creation and synchronization between nodes.
//!
//! Implements OFS-1300 (SSP) on top of `openfiat_gossip` and
//! `openfiat_registry`: providers register exactly the way any other
//! service does (`ServiceType::Infrastructure(InfrastructureService::SnapshotProvider)`,
//! per decision #9 — no separate registration event here, and no
//! standalone spec of its own), signed snapshot announcements travel as
//! gossip events (only metadata — §12), and every node derives its known
//! snapshot set purely by consuming them.
//!
//! The full lifecycle now closes here rather than stopping at metadata:
//!
//! - [`state`] serializes this node's persisted state canonically, and
//!   [`producer`] writes it to disk under [`config::SnapshotConfig`].
//! - [`location`] carries where those bytes can be fetched, inside the
//!   signed announcement — see [`events`] for why inside and not beside.
//! - [`reachable`] works out what that location should be, from the
//!   addresses the node has learned it answers on, so production needs no
//!   configuration at all.
//! - [`serve`] answers `GET /snapshot/{id}` from that directory,
//!   merged into the node's existing HTTP server.
//! - [`fetch`] downloads from an announced location and hands the bytes
//!   to [`store::SnapshotIndex::import`], which verifies size and state
//!   root, re-checks the producer's registration, and only then writes.
//!
//! §14's transport was previously declared out of scope, which left an
//! announced snapshot undownloadable and a joining node with no way to
//! avoid replaying all history. It is in scope now.

pub mod codec;
pub mod config;
pub mod error;
pub mod events;
pub mod fetch;
pub mod location;
pub mod producer;
pub mod protocol;
pub mod reachable;
pub mod record;
pub mod serve;
pub mod service;
pub mod state;
pub mod store;
pub mod trust;

pub use config::SnapshotConfig;
pub use error::SnapshotError;
pub use location::SnapshotLocation;
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
