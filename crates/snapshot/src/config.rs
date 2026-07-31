//! Operator-set snapshot policy: how often this node writes one, where it
//! keeps them, and — only when the node cannot work it out itself — where
//! it tells the cluster to fetch them from.
//!
//! All of it is genuine configuration under `openfiat-node`'s own rule
//! (see that crate's module doc): none of it can make two honest nodes
//! running the same release disagree about anything. A slower interval
//! produces fewer snapshots, a different directory keeps them elsewhere,
//! and a different URL points at a different mirror — the state root
//! decides what is true either way.
//!
//! # What is no longer configuration
//!
//! The *location* used to be, and its absence disabled snapshot production
//! entirely. It is now derived from the addresses the node has learned it
//! is reachable at (see [`crate::reachable`]), so production is on by
//! default and needs nothing set. What remains configurable is the
//! frequency, and an override for the node that genuinely cannot derive a
//! URL.

use crate::location::SnapshotLocation;
use crate::reachable;
use openfiat_network::Multiaddr;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

/// `[PROPOSED — NEEDS SIGN-OFF]`: the default production cadence.
/// OFS-1300 gives no interval; an hour is chosen so a joining node's
/// post-snapshot gossip catch-up is bounded by an hour of events, while
/// the cost — one full read of every domain column family — stays
/// negligible against a cluster of this size.
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
    /// How often this node writes and announces a snapshot. `None`
    /// switches production off for an operator who cannot spare the disk
    /// or the read.
    pub interval: Option<Duration>,
    /// An operator's override for where peers fetch this node's snapshots,
    /// used *instead of* anything derived.
    ///
    /// Empty is the ordinary case. This exists for the node whose HTTP
    /// server is reached on a hostname or port it cannot observe — a
    /// reverse proxy terminating TLS on 443 and forwarding to 7080, say.
    /// That node's derived `http://<its own ip>:7080` would be wrong, and
    /// it is the only one that knows so.
    pub public_urls: Vec<SnapshotLocation>,
    /// The socket this node's HTTP server binds, which is where a derived
    /// location gets its port — and, when it is loopback, the reason there
    /// is no derived location at all.
    ///
    /// `None` derives nothing. That is right for a `SnapshotConfig`
    /// assembled without a running server (tests, defaults); the node's
    /// composition root always sets it.
    pub rpc_bind: Option<SocketAddr>,
    pub retain: usize,
    /// Who this node will take a *first* snapshot from, when it holds no
    /// checkpoint of its own to judge one against.
    ///
    /// Configuration only in the additive sense — see [`crate::trust`].
    /// The pinned anchors are always present; an operator can add to them
    /// and cannot remove them.
    pub trusted_providers: crate::trust::TrustAnchors,
}

impl SnapshotConfig {
    /// Whether this node should produce and announce snapshots at all.
    ///
    /// Only the interval decides. A missing location no longer disables
    /// production, because a node with no configured location usually has
    /// a perfectly good derived one — see [`locations`](Self::locations),
    /// which is what reports having nowhere to serve from, at the moment a
    /// snapshot would be written rather than at startup.
    pub fn produces(&self) -> bool {
        self.interval.is_some()
    }

    /// Where this node will tell the cluster to fetch the snapshot it is
    /// about to write, given the addresses it has learned it is reachable
    /// at.
    ///
    /// The override wins outright rather than being merged: an operator
    /// who says "reach me here" has said something the node cannot check,
    /// and appending a derived guess after it would put an address the
    /// operator specifically corrected back into the announcement.
    ///
    /// A loopback RPC bind derives nothing. The HTTP server answers on
    /// this host only, so every derived location would be a URL that
    /// resolves, on each peer that tries it, to that peer itself. Such a
    /// node is behind a proxy, and the override is how it says where.
    pub fn locations(&self, reachable: &[Multiaddr]) -> Vec<SnapshotLocation> {
        if !self.public_urls.is_empty() {
            return self.public_urls.clone();
        }
        match self.rpc_bind {
            Some(bind) if !bind.ip().is_loopback() => reachable::base_urls(reachable, bind.port()),
            _ => Vec::new(),
        }
    }
}

impl Default for SnapshotConfig {
    fn default() -> Self {
        Self {
            directory: PathBuf::from("./openfiat-snapshots"),
            interval: Some(DEFAULT_INTERVAL),
            public_urls: Vec::new(),
            rpc_bind: None,
            retain: DEFAULT_RETAIN,
            trusted_providers: crate::trust::TrustAnchors::pinned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reachable(raw: &[&str]) -> Vec<Multiaddr> {
        raw.iter().map(|a| a.parse().unwrap()).collect()
    }

    fn bound(address: &str) -> SnapshotConfig {
        SnapshotConfig {
            rpc_bind: Some(address.parse().unwrap()),
            ..SnapshotConfig::default()
        }
    }

    /// The flag's removal, stated as a test: nothing is configured and the
    /// node still produces.
    #[test]
    fn a_default_node_produces() {
        assert!(SnapshotConfig::default().produces());
    }

    #[test]
    fn a_learned_address_is_enough_to_announce_from() {
        let config = bound("0.0.0.0:7080");
        let locations = config.locations(&reachable(&["/ip4/203.0.113.9/udp/4001/quic-v1"]));
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].as_str(), "http://203.0.113.9:7080");
    }

    #[test]
    fn an_override_replaces_the_derived_locations_rather_than_joining_them() {
        let config = SnapshotConfig {
            public_urls: vec![SnapshotLocation::parse("https://archive.example").unwrap()],
            ..bound("0.0.0.0:7080")
        };
        let locations = config.locations(&reachable(&["/ip4/203.0.113.9/udp/4001/quic-v1"]));
        assert_eq!(
            locations.len(),
            1,
            "the address the operator corrected must not come back"
        );
        assert_eq!(locations[0].as_str(), "https://archive.example");
    }

    #[test]
    fn a_node_with_no_learned_address_yet_has_nowhere_to_serve_from() {
        assert!(bound("0.0.0.0:7080").locations(&[]).is_empty());
    }

    #[test]
    fn a_loopback_rpc_bind_derives_nothing() {
        let config = bound("127.0.0.1:7080");
        assert!(
            config
                .locations(&reachable(&["/ip4/203.0.113.9/udp/4001/quic-v1"]))
                .is_empty(),
            "a server listening only on loopback is reachable by no peer, whatever address \
             the gossip transport learned"
        );
    }

    /// A loopback bind is the reverse-proxy case, and the override is
    /// exactly what such a node is expected to set.
    #[test]
    fn a_loopback_rpc_bind_still_honours_an_override() {
        let config = SnapshotConfig {
            public_urls: vec![SnapshotLocation::parse("https://archive.example").unwrap()],
            ..bound("127.0.0.1:7080")
        };
        assert_eq!(config.locations(&[]).len(), 1);
    }
}
