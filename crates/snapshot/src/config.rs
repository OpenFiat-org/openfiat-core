//! Operator-set snapshot policy: how often this node writes one, where
//! it keeps them, and the base URL it tells the rest of the cluster to
//! fetch them from.
//!
//! All three are genuine configuration under `openfiat-node`'s own rule
//! (see that crate's module doc): none of them can make two honest nodes
//! running the same release disagree about anything. A slower interval
//! produces fewer snapshots, a different directory keeps them elsewhere,
//! and a different public URL points at a different mirror — the state
//! root decides what is true either way.

use crate::location::SnapshotLocation;
use std::path::PathBuf;
use std::time::Duration;

/// `[PROPOSED — NEEDS SIGN-OFF]`: the default production cadence for a
/// node that has opted in. OFS-1300 gives no interval; an hour is chosen
/// so a joining node's post-snapshot gossip catch-up is bounded by an
/// hour of events, while the cost — one full read of every domain column
/// family — stays negligible against a cluster of this size.
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// How many produced snapshots stay on disk. Three gives a mirror
/// something to serve while the newest is still being written and leaves
/// a fallback if the newest turns out to be unreachable, without letting
/// an unattended node fill its own disk — the failure mode an unbounded
/// directory reaches on a long-running archival node and nowhere else.
pub const DEFAULT_RETAIN: usize = 3;

#[derive(Debug, Clone)]
pub struct SnapshotConfig {
    /// Where produced snapshots are written, and the only directory
    /// [`crate::serve`] will read from.
    pub directory: PathBuf,
    /// `None` disables production entirely — the zero-config default. A
    /// node that only *consumes* snapshots needs nothing here.
    pub interval: Option<Duration>,
    /// This node's own publicly reachable base URL(s), e.g.
    /// `http://archive.example:7080`. A node cannot discover this for
    /// itself: it sees a bind address, not what NAT, a load balancer, or
    /// a reverse proxy makes of it, and announcing a guess sends every
    /// joining node somewhere unreachable. So the operator states it, or
    /// this node does not announce.
    pub public_urls: Vec<SnapshotLocation>,
    pub retain: usize,
}

impl SnapshotConfig {
    /// Whether this node should produce and announce snapshots at all.
    ///
    /// Both halves are required, deliberately. Producing without a public
    /// URL would announce a snapshot nobody can fetch — precisely the
    /// state this crate is being fixed out of — so a missing URL disables
    /// production rather than degrading it.
    pub fn produces(&self) -> bool {
        self.interval.is_some() && !self.public_urls.is_empty()
    }
}

impl Default for SnapshotConfig {
    fn default() -> Self {
        Self {
            directory: PathBuf::from("./openfiat-snapshots"),
            interval: None,
            public_urls: Vec::new(),
            retain: DEFAULT_RETAIN,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_default_node_does_not_produce() {
        assert!(!SnapshotConfig::default().produces());
    }

    #[test]
    fn an_interval_without_a_public_url_still_does_not_produce() {
        let config = SnapshotConfig {
            interval: Some(DEFAULT_INTERVAL),
            ..SnapshotConfig::default()
        };
        assert!(
            !config.produces(),
            "announcing a snapshot with nowhere to fetch it is the bug being fixed"
        );
    }

    #[test]
    fn an_interval_and_a_public_url_produce() {
        let config = SnapshotConfig {
            interval: Some(DEFAULT_INTERVAL),
            public_urls: vec![SnapshotLocation::parse("http://archive.example:7080").unwrap()],
            ..SnapshotConfig::default()
        };
        assert!(config.produces());
    }
}
