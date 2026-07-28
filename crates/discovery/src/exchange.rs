//! Peer Exchange payloads (OFS-1100 §9), carried as the payload of an
//! `openfiat_network::Envelope` rather than a dedicated libp2p protocol —
//! OFNP §20 explicitly wants multiple protocol services sharing one
//! multiplexed connection, and the envelope is exactly that shared carrier.

use openfiat_types::{NodeRole, PeerId, PublicKey};

pub const OFS_SPEC: u16 = 1100;
pub const MESSAGE_TYPE_REQUEST: &str = "PeerExchangeRequest";
pub const MESSAGE_TYPE_RESPONSE: &str = "PeerExchangeResponse";
/// An unsolicited push of known peers, sent when a node learns something
/// genuinely new and wants to propagate it beyond one hop without waiting
/// for the next pull-based request cycle.
pub const MESSAGE_TYPE_ANNOUNCEMENT: &str = "PeerAnnouncement";

/// "Node A knows 120 peers, shares 20 random healthy peers with Node B" (§9)
/// — `max_peers` is B's request for how many A should share.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExchangeRequest {
    pub max_peers: u32,
}

/// What a peer exchange advertises about one node.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PeerAdvert {
    pub peer_id: PeerId,
    pub public_key: PublicKey,
    pub addresses: Vec<String>,
    pub roles: Vec<NodeRole>,
    pub node_version: String,
    pub supported_ofs: Vec<u16>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExchangeResponse {
    pub peers: Vec<PeerAdvert>,
}
