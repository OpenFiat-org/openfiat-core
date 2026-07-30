//! Node-level methods — Solana's `getVersion`/`getHealth` equivalents,
//! plus the peers this node has discovered.

use crate::dispatch::{MethodTable, method_fn};
use crate::error::RpcError;
use crate::state::NodeState;
use openfiat_storage::KvStore;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct VersionResult {
    pub version: &'static str,
}

/// One peer, as this node's discovery cache knows it.
///
/// Every field is one `PeerRecord` genuinely carries. There is
/// deliberately no uptime percentage and no health score: `successes` and
/// `failures` are this node's own count of exchanges with that peer, which
/// is a real measurement of a real thing, and folding them into a score
/// would present one node's local experience as a network-wide verdict.
///
/// The public key is included because it is what makes a peer's future
/// events verifiable, and it is not a secret — a peer id already encodes
/// it (see `openfiat_network::identity::public_key_from_peer_id`).
#[derive(Debug, Serialize)]
pub struct PeerView {
    pub peer_id: String,
    pub addresses: Vec<String>,
    pub node_version: String,
    pub supported_ofs: Vec<u16>,
    pub roles: Vec<openfiat_types::NodeRole>,
    pub last_seen: openfiat_types::Timestamp,
    pub latency_ms: Option<u32>,
    /// Exchanges with this peer that worked, and that did not, as counted
    /// by this node alone. Two nodes can honestly disagree about both.
    pub successes: u32,
    pub failures: u32,
}

#[derive(Debug, Serialize)]
pub struct PeersResult {
    /// This node's own identity, in the form that goes in a multiaddr.
    ///
    /// Reported because an operator publishing an entrypoint needs it and
    /// has nowhere else to get it: `/ip4/<host>/udp/4001/quic-v1/p2p/<this>`
    /// is the string they hand to other operators, and assembling it from
    /// a log line is how it gets typed wrong.
    pub self_peer_id: String,
    pub peers: Vec<PeerView>,
    /// The addresses this node asks peers to dial it at — operator-declared
    /// ones first, then bound ones.
    ///
    /// Worth exposing because "my node announces nothing" was invisible
    /// from outside for as long as it was true, and an operator checking
    /// whether their `--external-addr` took effect has nowhere else to
    /// look.
    pub announced_addresses: Vec<String>,
}

pub fn register<S: KvStore + 'static>(table: &mut MethodTable<S>) {
    table.register(
        "getVersion",
        method_fn(
            |_state: &NodeState<S>,
             _params: serde_json::Value|
             -> Result<VersionResult, RpcError> {
                Ok(VersionResult {
                    version: crate::version(),
                })
            },
        ),
    );
    table.register(
        "getPeers",
        method_fn(
            |state: &NodeState<S>, _params: serde_json::Value| -> Result<PeersResult, RpcError> {
                let discovery = state.discovery.borrow();
                let peers = discovery
                    .cache
                    .all()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|record| PeerView {
                        // The `12D3Koo…` form an operator can paste into
                        // an `--entrypoint`, not the byte array the wire
                        // format uses.
                        peer_id: openfiat_network::identity::readable_peer_id(&record.peer_id)
                            .unwrap_or_default(),
                        addresses: record.addresses,
                        node_version: record.node_version,
                        supported_ofs: record.supported_ofs,
                        roles: record.roles,
                        last_seen: record.last_seen,
                        latency_ms: record.latency_ms,
                        successes: record.successes,
                        failures: record.failures,
                    })
                    .collect();
                Ok(PeersResult {
                    self_peer_id: state.gossip.borrow().node.libp2p_peer_id().to_string(),
                    peers,
                    announced_addresses: discovery.announced_addresses(),
                })
            },
        ),
    );
    table.register(
        "getHealth",
        method_fn(
            |_state: &NodeState<S>, _params: serde_json::Value| -> Result<&'static str, RpcError> {
                Ok("ok")
            },
        ),
    );
}
