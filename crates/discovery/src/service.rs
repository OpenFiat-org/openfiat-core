//! Drives one node's peer discovery.
//!
//! A single request/response round per connection isn't enough to
//! propagate discoveries beyond one hop — if node A learns about C from B
//! *after* already answering A's own peer-exchange request, A never gets
//! another chance to learn about C unless something tells it again. So on
//! top of §9's pull-based exchange (ask a peer what it knows), this also
//! pushes a `PeerAnnouncement` to every other connected peer whenever a
//! genuinely new peer is learned, so knowledge propagates through the mesh
//! in the same round it's discovered rather than only on the next request.
//! This is what makes §17 ("bootstrap independence") and §22 (partition
//! recovery) actually converge quickly instead of eventually-after-many-
//! independent-retries.

use crate::cache::PeerCache;
use crate::exchange::{ExchangeRequest, ExchangeResponse, MESSAGE_TYPE_ANNOUNCEMENT, MESSAGE_TYPE_REQUEST, MESSAGE_TYPE_RESPONSE, OFS_SPEC, PeerAdvert};
use crate::record::PeerRecord;
use libp2p::request_response::{self, Message, ResponseChannel};
use libp2p::swarm::SwarmEvent;
use openfiat_network::behaviour::OpenFiatBehaviourEvent;
use openfiat_network::identity::from_libp2p_peer_id;
use openfiat_network::{Envelope, Multiaddr, Node, PeerId as Libp2pPeerId};
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{NodeRole, PeerId, PublicKey};
use std::collections::HashSet;

/// How many peers to request/announce in one exchange, and the default cap
/// on how many total peers a node dials up to (§12's "Preferred" tier,
/// scaled down for small test clusters — real deployments configure this
/// explicitly rather than relying on the default).
const DEFAULT_MAX_EXCHANGE_PEERS: u32 = 20;

pub struct DiscoveryService<S> {
    pub node: Node,
    pub cache: PeerCache<S>,
    self_peer_id: PeerId,
    self_public_key: PublicKey,
    self_node_version: String,
    self_supported_ofs: Vec<u16>,
    self_roles: Vec<NodeRole>,
    self_addresses: Vec<String>,
    connected: HashSet<Libp2pPeerId>,
    target_peers: usize,
}

impl<S: KvStore> DiscoveryService<S> {
    pub fn new(
        node: Node,
        cache: PeerCache<S>,
        self_public_key: PublicKey,
        self_node_version: impl Into<String>,
        self_supported_ofs: Vec<u16>,
        self_roles: Vec<NodeRole>,
        target_peers: usize,
    ) -> Self {
        let self_peer_id = node.local_peer_id();
        Self {
            node,
            cache,
            self_peer_id,
            self_public_key,
            self_node_version: self_node_version.into(),
            self_supported_ofs,
            self_roles,
            self_addresses: Vec::new(),
            connected: HashSet::new(),
            target_peers,
        }
    }

    pub fn dial(&mut self, addr: Multiaddr) -> Result<(), openfiat_network::NetworkError> {
        self.node.dial(addr)
    }

    /// Addresses this node has learned it's actually listening on
    /// (populated as `NewListenAddr` events arrive).
    pub fn listen_addresses(&self) -> &[String] {
        &self.self_addresses
    }

    /// Wait for and process exactly one swarm event.
    pub async fn drive_once(&mut self) {
        let event = self.node.next_event().await;
        self.handle(event);
    }

    fn handle(&mut self, event: SwarmEvent<OpenFiatBehaviourEvent>) {
        match event {
            SwarmEvent::NewListenAddr { address, .. } => {
                self.self_addresses.push(address.to_string());
            }
            SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                self.on_connected(peer_id);
            }
            SwarmEvent::ConnectionClosed { peer_id, .. } => {
                self.connected.remove(&peer_id);
            }
            SwarmEvent::Behaviour(OpenFiatBehaviourEvent::Envelope(request_response::Event::Message {
                peer,
                message,
                ..
            })) => match message {
                Message::Request { request, channel, .. } => self.on_request(peer, request, channel),
                Message::Response { response, .. } => self.on_response(peer, response),
            },
            _ => {}
        }
    }

    /// Self, plus every cached peer with a real (non-placeholder) address,
    /// excluding `exclude` — the shape both a pull response and a push
    /// announcement share.
    fn advert_batch(&self, exclude: &PeerId) -> Vec<PeerAdvert> {
        let mut peers = vec![PeerAdvert {
            peer_id: self.self_peer_id.clone(),
            public_key: self.self_public_key,
            addresses: self.self_addresses.clone(),
            roles: self.self_roles.clone(),
            node_version: self.self_node_version.clone(),
            supported_ofs: self.self_supported_ofs.clone(),
        }];
        let known = self.cache.healthiest(DEFAULT_MAX_EXCHANGE_PEERS as usize).unwrap_or_default();
        peers.extend(known.into_iter().filter(|record| &record.peer_id != exclude && !record.addresses.is_empty()).map(
            |record| PeerAdvert {
                peer_id: record.peer_id,
                public_key: record.public_key,
                addresses: record.addresses,
                roles: record.roles,
                node_version: record.node_version,
                supported_ofs: record.supported_ofs,
            },
        ));
        peers
    }

    /// Absorb a batch of peer adverts (from a pull response or a push
    /// announcement): cache each one, dial genuinely new peers, and report
    /// which ones were new so the caller can decide whether to propagate
    /// further.
    fn learn_peers(&mut self, adverts: Vec<PeerAdvert>) -> Vec<PeerId> {
        let mut newly_learned = Vec::new();
        for advert in adverts {
            if advert.peer_id == self.self_peer_id {
                continue;
            }
            let already_known = self.cache.get(&advert.peer_id).ok().flatten().is_some_and(|r| !r.addresses.is_empty());

            let record = PeerRecord::new(
                advert.peer_id.clone(),
                advert.public_key,
                advert.addresses.clone(),
                advert.node_version,
                advert.supported_ofs,
                advert.roles,
            );
            let _ = self.cache.upsert(&record);

            if !already_known {
                newly_learned.push(advert.peer_id.clone());
                let under_target = self.cache.all().map(|all| all.len()).unwrap_or(0) <= self.target_peers;
                if under_target
                    && let Some(addr) = advert.addresses.first().and_then(|addr| addr.parse::<Multiaddr>().ok())
                {
                    let _ = self.node.dial(addr);
                }
            }
        }
        newly_learned
    }

    /// Push an announcement of everything we know to every connected peer
    /// except `exclude` (typically whoever we just learned it from).
    fn broadcast_announcement(&mut self, exclude: Libp2pPeerId) {
        let excluded_peer_id = from_libp2p_peer_id(exclude);
        let peers = self.advert_batch(&excluded_peer_id);
        let payload = wire::to_bytes(&ExchangeResponse { peers }).expect("ExchangeResponse always serializes");
        for peer in self.connected.clone() {
            if peer == exclude {
                continue;
            }
            self.node.send_envelope(peer, Envelope::new(OFS_SPEC, MESSAGE_TYPE_ANNOUNCEMENT, 1, payload.clone()));
        }
    }

    fn on_connected(&mut self, peer: Libp2pPeerId) {
        self.connected.insert(peer);
        let peer_id = from_libp2p_peer_id(peer);
        if self.cache.get(&peer_id).ok().flatten().is_none() {
            let placeholder = PeerRecord::new(peer_id, PublicKey::from_bytes([0u8; 32]), Vec::new(), String::new(), Vec::new(), Vec::new());
            let _ = self.cache.upsert(&placeholder);
        }

        let request = ExchangeRequest { max_peers: DEFAULT_MAX_EXCHANGE_PEERS };
        let payload = wire::to_bytes(&request).expect("ExchangeRequest always serializes");
        self.node.send_envelope(peer, Envelope::new(OFS_SPEC, MESSAGE_TYPE_REQUEST, 1, payload));
    }

    fn on_request(&mut self, peer: Libp2pPeerId, envelope: Envelope, channel: ResponseChannel<Envelope>) {
        if envelope.header.message_type == MESSAGE_TYPE_ANNOUNCEMENT {
            if let Ok(announcement) = wire::from_bytes::<ExchangeResponse>(&envelope.payload) {
                let newly_learned = self.learn_peers(announcement.peers);
                if !newly_learned.is_empty() {
                    self.broadcast_announcement(peer);
                }
            }
            // Announcements are fire-and-forget; dropping `channel` is a
            // valid, harmless outcome (OFNP request-response semantics).
            return;
        }
        if envelope.header.message_type != MESSAGE_TYPE_REQUEST {
            return;
        }

        let requester = from_libp2p_peer_id(peer);
        let payload = wire::to_bytes(&ExchangeResponse { peers: self.advert_batch(&requester) }).expect("ExchangeResponse always serializes");
        let response = Envelope::new(OFS_SPEC, MESSAGE_TYPE_RESPONSE, 1, payload);
        let _ = self.node.swarm.behaviour_mut().envelope.send_response(channel, response);
    }

    fn on_response(&mut self, peer: Libp2pPeerId, envelope: Envelope) {
        if envelope.header.message_type != MESSAGE_TYPE_RESPONSE {
            return;
        }
        let Ok(response) = wire::from_bytes::<ExchangeResponse>(&envelope.payload) else {
            return;
        };
        let newly_learned = self.learn_peers(response.peers);
        if !newly_learned.is_empty() {
            self.broadcast_announcement(peer);
        }
    }
}
