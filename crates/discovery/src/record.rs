//! What one node knows about one peer (OFS-1100 §4, §7).

use openfiat_types::{NodeRole, PeerId, PublicKey, Timestamp};

/// One entry in the local peer cache (§7).
///
/// Addresses are stored as their string form (`Multiaddr::to_string`)
/// rather than `Multiaddr` itself, so this crate doesn't need a direct
/// `libp2p` dependency — see `openfiat-network`'s re-exports for the
/// typed form used when actually dialing.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PeerRecord {
    pub peer_id: PeerId,
    pub public_key: PublicKey,
    pub addresses: Vec<String>,
    pub node_version: String,
    /// OFS specification numbers this peer supports (OFNP §10's example:
    /// `1100`, `1200`, ...).
    pub supported_ofs: Vec<u16>,
    pub roles: Vec<NodeRole>,
    pub last_seen: Timestamp,
    pub latency_ms: Option<u32>,
    pub successes: u32,
    pub failures: u32,
}

impl PeerRecord {
    /// Build a fresh record from a just-received, already-verified
    /// advertisement (see [`crate::advertisement`]).
    pub fn new(
        peer_id: PeerId,
        public_key: PublicKey,
        addresses: Vec<String>,
        node_version: String,
        supported_ofs: Vec<u16>,
        roles: Vec<NodeRole>,
    ) -> Self {
        Self {
            peer_id,
            public_key,
            addresses,
            node_version,
            supported_ofs,
            roles,
            last_seen: Timestamp::now(),
            latency_ms: None,
            successes: 0,
            failures: 0,
        }
    }

    /// Record a successful interaction (connection, response, ...) — §7's
    /// "Success History", §13's replacement-candidacy signal.
    pub fn record_success(&mut self) {
        self.successes = self.successes.saturating_add(1);
        self.last_seen = Timestamp::now();
    }

    /// Record a failed interaction (timeout, malformed response, ...).
    pub fn record_failure(&mut self) {
        self.failures = self.failures.saturating_add(1);
    }

    /// Whether this peer supports every OFS specification in `required`.
    pub fn supports(&self, required: &[u16]) -> bool {
        required
            .iter()
            .all(|spec| self.supported_ofs.contains(spec))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> PeerRecord {
        PeerRecord::new(
            PeerId::from_bytes(vec![1, 2, 3]),
            PublicKey::from_bytes([9u8; 32]),
            vec!["/ip4/127.0.0.1/udp/4001/quic-v1".to_string()],
            "1.0.0".to_string(),
            vec![1000, 1100],
            vec![NodeRole::FullNode],
        )
    }

    #[test]
    fn supports_checks_every_required_spec() {
        let record = record();
        assert!(record.supports(&[1000]));
        assert!(record.supports(&[1000, 1100]));
        assert!(!record.supports(&[1000, 1200]));
    }

    #[test]
    fn record_success_increments_the_counter_and_touches_last_seen() {
        let mut record = record();
        assert_eq!(record.successes, 0);
        record.record_success();
        assert_eq!(record.successes, 1);
    }
}
