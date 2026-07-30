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
use crate::exchange::{
    ExchangeRequest, ExchangeResponse, MESSAGE_TYPE_ANNOUNCEMENT, MESSAGE_TYPE_REQUEST,
    MESSAGE_TYPE_RESPONSE, OFS_SPEC, PeerAdvert,
};
use crate::record::PeerRecord;
use libp2p::request_response::{Message, ResponseChannel};
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

/// # Why this does not own a swarm
///
/// It used to hold its own [`Node`], and that is exactly why it never ran:
/// a node has one libp2p swarm, only one thing can drive that swarm's
/// event loop, and the actor gave the loop to `GossipService`. A service
/// holding a second swarm nobody drives receives nothing, for ever, while
/// looking perfectly well-formed.
///
/// So the swarm is passed in where it is needed. The actor owns the loop,
/// reads one event, and routes it — discovery's envelopes carry OFS spec
/// 1100 ([`crate::exchange::OFS_SPEC`]), which is what the field is for —
/// and hands this service the same `Node` gossip uses. One connection,
/// one identity, two protocols multiplexed over it, which is what OFNP
/// §20 asks for.
pub struct DiscoveryService<S> {
    pub cache: PeerCache<S>,
    self_peer_id: PeerId,
    self_public_key: PublicKey,
    self_node_version: String,
    self_supported_ofs: Vec<u16>,
    self_roles: Vec<NodeRole>,
    /// Addresses libp2p reports this node is bound to.
    self_addresses: Vec<String>,
    /// Addresses an operator has declared this node is reachable at from
    /// outside its own network. See [`Self::add_external_address`].
    external_addresses: Vec<String>,
    connected: HashSet<Libp2pPeerId>,
    target_peers: usize,
}

impl<S: KvStore> DiscoveryService<S> {
    pub fn new(
        self_peer_id: PeerId,
        cache: PeerCache<S>,
        self_public_key: PublicKey,
        self_node_version: impl Into<String>,
        self_supported_ofs: Vec<u16>,
        self_roles: Vec<NodeRole>,
        target_peers: usize,
    ) -> Self {
        Self {
            cache,
            self_peer_id,
            self_public_key,
            self_node_version: self_node_version.into(),
            self_supported_ofs,
            self_roles,
            self_addresses: Vec::new(),
            external_addresses: Vec::new(),
            connected: HashSet::new(),
            target_peers,
        }
    }

    /// Addresses this node has learned it's actually listening on
    /// (populated as `NewListenAddr` events arrive).
    pub fn listen_addresses(&self) -> &[String] {
        &self.self_addresses
    }

    /// Declares an address peers should use to reach this node.
    ///
    /// # Why a node cannot work this out for itself
    ///
    /// `NewListenAddr` reports the socket libp2p actually bound, which is an
    /// address on one of this machine's own interfaces. Behind NAT, inside a
    /// container, or on a cloud host with a mapped public IP, that address is
    /// private and no peer outside the local network can dial it. The node
    /// has no way to observe its own public address — by construction, only
    /// something on the far side of the NAT can see it.
    ///
    /// So it is declared by the operator rather than guessed. A node that is
    /// genuinely on a public interface needs nothing here: its bound address
    /// already is its public one.
    ///
    /// Announced *in addition to* the bound addresses, not instead of them.
    /// Peers on the same private network (a docker-compose cluster, a LAN)
    /// can only reach each other by the private address, so dropping those
    /// would break the local case in order to fix the remote one.
    pub fn add_external_address(&mut self, address: impl Into<String>) {
        let address = address.into();
        if !self.external_addresses.contains(&address) {
            self.external_addresses.push(address);
        }
    }

    /// Every address this node asks peers to dial: operator-declared ones
    /// first, then the bound ones.
    ///
    /// Declared addresses come first because a peer that tries them in order
    /// reaches a NAT'd node on the first attempt rather than after timing out
    /// on an unreachable private address.
    ///
    /// Only the unspecified wildcard is filtered — see [`is_announceable`],
    /// which explains why loopback and private ranges are deliberately
    /// announced rather than excluded.
    pub fn announced_addresses(&self) -> Vec<String> {
        self.external_addresses
            .iter()
            .chain(self.self_addresses.iter())
            .filter(|addr| is_announceable(addr))
            .cloned()
            .collect()
    }

    /// Connection and listen-address events, which belong to no single
    /// protocol.
    ///
    /// Taken by reference because the other services on this swarm need
    /// the same event. Only the envelope messages have a single owner,
    /// and those are routed by spec number — see [`Self::owns`].
    pub fn handle_lifecycle(
        &mut self,
        event: &SwarmEvent<OpenFiatBehaviourEvent>,
        node: &mut Node,
    ) {
        match event {
            SwarmEvent::NewListenAddr { address, .. } => {
                self.self_addresses.push(address.to_string());
            }
            SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                self.on_connected(*peer_id, node);
            }
            SwarmEvent::ConnectionClosed { peer_id, .. } => {
                self.connected.remove(peer_id);
            }
            _ => {}
        }
    }

    /// Whether an envelope on the shared request-response protocol is this
    /// service's.
    ///
    /// The spec number, not a guess. Several protocols share one envelope
    /// carrier by OFNP §20's design, and `ofs_spec` is the field that says
    /// whose a message is — routing on anything else (message type, payload
    /// shape) would be inferring what is already stated.
    pub fn owns(envelope: &Envelope) -> bool {
        envelope.header.ofs_spec == OFS_SPEC
    }

    /// One peer-exchange message. The caller has already established it is
    /// ours, via [`Self::owns`].
    pub fn handle_message(
        &mut self,
        peer: Libp2pPeerId,
        message: Message<Envelope, Envelope>,
        node: &mut Node,
    ) {
        match message {
            Message::Request {
                request, channel, ..
            } => self.on_request(peer, request, channel, node),
            Message::Response { response, .. } => self.on_response(peer, response, node),
        }
    }

    /// Self, plus every cached peer with a real (non-placeholder) address,
    /// excluding `exclude` — the shape both a pull response and a push
    /// announcement share.
    fn advert_batch(&self, exclude: &PeerId) -> Vec<PeerAdvert> {
        let mut peers = vec![PeerAdvert {
            peer_id: self.self_peer_id.clone(),
            public_key: self.self_public_key,
            addresses: self.announced_addresses(),
            roles: self.self_roles.clone(),
            node_version: self.self_node_version.clone(),
            supported_ofs: self.self_supported_ofs.clone(),
        }];
        let known = self
            .cache
            .healthiest(DEFAULT_MAX_EXCHANGE_PEERS as usize)
            .unwrap_or_default();
        peers.extend(
            known
                .into_iter()
                .filter(|record| &record.peer_id != exclude && !record.addresses.is_empty())
                .map(|record| PeerAdvert {
                    peer_id: record.peer_id,
                    public_key: record.public_key,
                    addresses: record.addresses,
                    roles: record.roles,
                    node_version: record.node_version,
                    supported_ofs: record.supported_ofs,
                }),
        );
        peers
    }

    /// Absorb a batch of peer adverts (from a pull response or a push
    /// announcement): cache each one, dial genuinely new peers, and report
    /// which ones were new so the caller can decide whether to propagate
    /// further.
    fn learn_peers(&mut self, adverts: Vec<PeerAdvert>, node: &mut Node) -> Vec<PeerId> {
        let mut newly_learned = Vec::new();
        for advert in adverts {
            if advert.peer_id == self.self_peer_id {
                continue;
            }
            let already_known = self
                .cache
                .get(&advert.peer_id)
                .ok()
                .flatten()
                .is_some_and(|r| !r.addresses.is_empty());

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
                let under_target =
                    self.cache.all().map(|all| all.len()).unwrap_or(0) <= self.target_peers;
                if under_target
                    && let Some(addr) = advert
                        .addresses
                        .first()
                        .and_then(|addr| addr.parse::<Multiaddr>().ok())
                {
                    let _ = node.dial(addr);
                }
            }
        }
        newly_learned
    }

    /// Push an announcement of everything we know to every connected peer
    /// except `exclude` (typically whoever we just learned it from).
    fn broadcast_announcement(&mut self, exclude: Libp2pPeerId, node: &mut Node) {
        let excluded_peer_id = from_libp2p_peer_id(exclude);
        let peers = self.advert_batch(&excluded_peer_id);
        let payload = wire::to_bytes(&ExchangeResponse { peers })
            .expect("ExchangeResponse always serializes");
        for peer in self.connected.clone() {
            if peer == exclude {
                continue;
            }
            node.send_envelope(
                peer,
                Envelope::new(OFS_SPEC, MESSAGE_TYPE_ANNOUNCEMENT, 1, payload.clone()),
            );
        }
    }

    fn on_connected(&mut self, peer: Libp2pPeerId, node: &mut Node) {
        self.connected.insert(peer);
        let peer_id = from_libp2p_peer_id(peer);
        if self.cache.get(&peer_id).ok().flatten().is_none() {
            let placeholder = PeerRecord::new(
                peer_id,
                PublicKey::from_bytes([0u8; 32]),
                Vec::new(),
                String::new(),
                Vec::new(),
                Vec::new(),
            );
            let _ = self.cache.upsert(&placeholder);
        }

        let request = ExchangeRequest {
            max_peers: DEFAULT_MAX_EXCHANGE_PEERS,
        };
        let payload = wire::to_bytes(&request).expect("ExchangeRequest always serializes");
        node.send_envelope(
            peer,
            Envelope::new(OFS_SPEC, MESSAGE_TYPE_REQUEST, 1, payload),
        );
    }

    fn on_request(
        &mut self,
        peer: Libp2pPeerId,
        envelope: Envelope,
        channel: ResponseChannel<Envelope>,
        node: &mut Node,
    ) {
        if envelope.header.message_type == MESSAGE_TYPE_ANNOUNCEMENT {
            if let Ok(announcement) = wire::from_bytes::<ExchangeResponse>(&envelope.payload) {
                let newly_learned = self.learn_peers(announcement.peers, node);
                if !newly_learned.is_empty() {
                    self.broadcast_announcement(peer, node);
                }
            }
            // An announcement wants no reply, but the channel must still be
            // closed. libp2p holds an inbound stream open until the response
            // is written or the channel is dropped *and the handler notices*
            // — and a node at its inbound limit starts logging "Dropping
            // inbound stream because we are at capacity" and refusing real
            // requests. An earlier comment here called dropping the channel
            // "a valid, harmless outcome". It was not; the same mistake was
            // found and fixed in `openfiat-gossip` first.
            acknowledge(channel, node);
            return;
        }
        if envelope.header.message_type != MESSAGE_TYPE_REQUEST {
            return;
        }

        let requester = from_libp2p_peer_id(peer);
        let payload = wire::to_bytes(&ExchangeResponse {
            peers: self.advert_batch(&requester),
        })
        .expect("ExchangeResponse always serializes");
        let response = Envelope::new(OFS_SPEC, MESSAGE_TYPE_RESPONSE, 1, payload);
        let _ = node
            .swarm
            .behaviour_mut()
            .envelope
            .send_response(channel, response);
    }

    fn on_response(&mut self, peer: Libp2pPeerId, envelope: Envelope, node: &mut Node) {
        if envelope.header.message_type != MESSAGE_TYPE_RESPONSE {
            return;
        }
        let Ok(response) = wire::from_bytes::<ExchangeResponse>(&envelope.payload) else {
            return;
        };
        let newly_learned = self.learn_peers(response.peers, node);
        if !newly_learned.is_empty() {
            self.broadcast_announcement(peer, node);
        }
    }
}

/// Closes an inbound stream that wants no reply.
///
/// An empty envelope rather than a dropped channel: see the call site for
/// why a dropped one is not free.
fn acknowledge(channel: ResponseChannel<Envelope>, node: &mut Node) {
    let _ = node.swarm.behaviour_mut().envelope.send_response(
        channel,
        Envelope::new(OFS_SPEC, MESSAGE_TYPE_ANNOUNCEMENT, 1, Vec::new()),
    );
}

/// Whether an address is worth announcing to a peer.
///
/// Only the **unspecified** wildcard is excluded (`0.0.0.0`, `::`). It is a
/// bind directive meaning "every local interface", not a destination, and it
/// is exactly what `--gossip-bind-address` normally contains — so without this
/// filter it is precisely what a node announces, and no peer can dial it.
///
/// # Loopback is deliberately NOT filtered
///
/// An earlier version of this excluded `127.0.0.0/8` on the reasoning that
/// it "points a dialing peer at itself". That is true across machines and
/// false on one: processes on the same host reach each other over loopback
/// perfectly well. Filtering it broke the five-node convergence test, whose
/// cluster listens on `/ip4/127.0.0.1/udp/0/quic-v1` — every node announced
/// an empty address list and the cluster could not converge. A single-host
/// dev cluster is a real deployment, not just a test artifact.
///
/// Private ranges are likewise announced: a docker-compose cluster or a LAN
/// reaches its peers only by private address. An operator whose private
/// address is useless to outsiders declares an external one (see
/// [`DiscoveryService::add_external_address`]), which is announced first.
fn is_announceable(address: &str) -> bool {
    // Delegates so this crate and `openfiat-gossip` cannot drift apart on
    // what "dialable" means. They answer the same question for different
    // reasons — one decides what to announce, the other what to report to
    // an operator — and two copies of a filter like this stay identical
    // only until one of them is fixed.
    address
        .parse()
        .is_ok_and(|parsed| openfiat_network::identity::is_dialable(&parsed))
}

#[cfg(test)]
mod announce_tests {
    use super::is_announceable;

    #[test]
    fn refuses_the_bind_wildcard() {
        // The default --gossip-bind-address. Without this filter it is what every
        // node announces, and no peer can dial it.
        assert!(!is_announceable("/ip4/0.0.0.0/udp/4001/quic-v1"));
        assert!(!is_announceable("/ip6/::/udp/4001/quic-v1"));
    }

    // Filtering loopback broke the five-node convergence test: its cluster
    // listens on 127.0.0.1, so every node announced nothing and no peer had
    // an address to dial. Processes on one host reach each other over
    // loopback, and a single-host cluster is a real deployment.
    #[test]
    fn keeps_loopback_so_a_single_host_cluster_still_converges() {
        assert!(is_announceable("/ip4/127.0.0.1/udp/4001/quic-v1"));
        assert!(is_announceable("/ip6/::1/udp/4001/quic-v1"));
    }

    #[test]
    fn keeps_private_addresses_so_local_clusters_still_work() {
        // docker-compose's own subnet, and an ordinary LAN. Peers there can
        // reach each other only by these.
        assert!(is_announceable("/ip4/172.28.0.10/udp/4001/quic-v1"));
        assert!(is_announceable("/ip4/192.168.1.20/udp/4001/quic-v1"));
        assert!(is_announceable("/ip4/10.0.0.5/udp/4001/quic-v1"));
    }

    #[test]
    fn keeps_public_addresses() {
        assert!(is_announceable("/ip4/84.32.34.7/udp/4001/quic-v1"));
        assert!(is_announceable("/dns4/node.example.com/udp/4001/quic-v1"));
    }
}
