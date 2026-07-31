//! Telling the IPFS network which content this node has.
//!
//! # What this is for, and why bitswap alone was not enough
//!
//! A node serves blocks over bitswap (`openfiat_content::bitswap`) to any
//! peer that asks. Nothing made anyone ask. Bitswap has no way to find a
//! provider — it is what you speak *after* you know who has the content —
//! so a public gateway resolving an attachment CID had no route to an
//! OpenFiat node holding it, and the durability the content premium pays
//! for existed only between peers that already knew each other.
//!
//! Provider records are that route. A node publishes "I have this
//! multihash" into the public IPFS DHT, and anyone resolving the CID
//! finds it — a gateway, a browser running Helia, someone who has never
//! heard of OpenFiat. That last case is the point: it is what "provides
//! content to the IPFS network" actually requires, and no OpenFiat-only
//! mechanism can substitute for it.
//!
//! # Client mode, deliberately
//!
//! [`Mode::Client`] publishes our records and runs our queries, and
//! declines to answer routing queries for anyone else. Server mode would
//! make every OpenFiat node a general-purpose IPFS DHT node: a routing
//! table of the whole network in memory, and inbound query traffic from
//! strangers, in exchange for nothing OpenFiat needs. The network's
//! durability guarantee is about *our* content being findable, and client
//! mode delivers exactly that. An operator who wants to run a full DHT
//! node should run Kubo, which is built for it.
//!
//! # Keyed by multihash
//!
//! Not by CID — see [`Cid::multihash`]. IPFS has keyed providers by
//! multihash since 2020 so that one piece of content has one record
//! whichever codec names it.
//!
//! # The bootstrappers are literal, and that was not the obvious choice
//!
//! The canonical IPFS bootstrap list is written with `/dnsaddr/`, which
//! is a TXT-record indirection this workspace has no resolver for: libp2p
//! resolves it in the `dns` transport, which is deliberately not enabled
//! here because it pulls in `hickory-proto` and its advisory history (see
//! `crate::identity::resolve_dns_multiaddr`).
//!
//! The `/dnsaddr/` chain terminates in ordinary `/dns/` addresses, and
//! those the existing resolver already handles. So the list below is the
//! resolved leaves — `_dnsaddr.bootstrap.libp2p.io` expanded one level,
//! then each region expanded once more — rather than a DNS dependency or
//! a set of bare IPs that rot the day a host moves.
//!
//! Every entry keeps its `/p2p/` peer id, which is the whole security of
//! resolving a name at all: DNS is unauthenticated, and a hijacked record
//! points at a host that then fails the libp2p handshake. Dropping the
//! peer id would turn a hijack into a working connection to an attacker.

use crate::identity::resolve_dns_multiaddr;
use libp2p::kad::{self, store::MemoryStore};
use libp2p::{Multiaddr, PeerId as Libp2pPeerId, StreamProtocol};

/// This network's own DHT protocol name.
///
/// **Not** `/ipfs/kad/1.0.0`. Two Kademlia instances interoperate only if
/// their protocol names match, so this one string is what separates an
/// OpenFiat DHT from the public IPFS one — a peer that speaks the public
/// name cannot open a stream here at all, and this node never appears in
/// its routing table.
///
/// This node's content offering is not public. It was on the public DHT,
/// and that was wrong twice over. It disclosed to the whole IPFS network
/// which peer holds which trade attachment, and — because a libp2p swarm
/// has one connection set shared by every behaviour — it put several
/// thousand peers that do not speak this protocol into the same set that
/// gossip broadcasts over. Two nodes that were connected to each other
/// were each also connected to 3,500 strangers, and protocol events did
/// not reliably cross between them.
pub const OPENFIAT_KAD_PROTOCOL: StreamProtocol = StreamProtocol::new("/openfiat/kad/1.0.0");

/// Nothing. This DHT is bootstrapped from the node's own
/// `--entrypoint` peers, which are OpenFiat nodes.
///
/// It used to hold the libp2p Foundation's public bootstrappers. Dialling
/// them was how this node ended up on the public IPFS DHT; keeping the
/// list empty is what keeps the network private, and there is nowhere
/// else to bootstrap from because an OpenFiat node's peers are exactly
/// the peers it already dials.
pub const BOOTSTRAPPERS: &[&str] = &[];

/// The DHT behaviour a node runs: a server in this network's own DHT.
pub fn behaviour(local_peer_id: Libp2pPeerId) -> kad::Behaviour<MemoryStore> {
    let mut config = kad::Config::new(OPENFIAT_KAD_PROTOCOL);
    // A record is republished before the network expires it. IPFS drops a
    // provider record after 24 hours, and a node that only republished on
    // the hour it expired would be unfindable for whatever slice of an
    // hour the two clocks disagreed by.
    config.set_provider_publication_interval(Some(PROVIDER_REPUBLISH_INTERVAL));
    config.set_provider_record_ttl(Some(PROVIDER_RECORD_TTL));

    let mut behaviour =
        kad::Behaviour::with_config(local_peer_id, MemoryStore::new(local_peer_id), config);
    // Server, not client. Client mode was right while this node was a
    // guest on somebody else's DHT: it published records and answered
    // nothing, leaving storage to the public network. In a private DHT
    // there is no public network to lean on — these nodes *are* the DHT,
    // and a network of clients stores nothing for anybody.
    behaviour.set_mode(Some(kad::Mode::Server));
    behaviour
}

/// How often this node republishes what it provides.
///
/// Twelve hours against the network's twenty-four, which is what Kubo
/// does and for the same reason: one missed cycle must not make a node's
/// content unfindable.
pub const PROVIDER_REPUBLISH_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(12 * 60 * 60);

/// How long this node considers someone else's provider record good for.
///
/// The IPFS network's own figure. Set explicitly because it is a
/// statement about interoperating with everyone else's expiry, not a knob
/// to tune.
pub const PROVIDER_RECORD_TTL: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// The bootstrappers, resolved and paired with the peer id each address
/// names.
///
/// A hostname that does not resolve is dropped rather than fatal — one
/// unreachable bootstrapper out of five is a bad afternoon for somebody
/// else's DNS, not a reason for this node to refuse to start. An address
/// with no `/p2p/` component is dropped too, and that one *is* a
/// programming error: without it there is nothing for the handshake to
/// check the far side against.
pub fn resolved_bootstrappers() -> Vec<(Libp2pPeerId, Multiaddr)> {
    let mut resolved = Vec::new();
    for entry in BOOTSTRAPPERS {
        let Ok(address) = entry.parse::<Multiaddr>() else {
            continue;
        };
        let Some(peer) = crate::identity::peer_id_in_multiaddr(&address) else {
            debug_assert!(false, "a bootstrapper without a peer id: {entry}");
            continue;
        };
        match resolve_dns_multiaddr(&address) {
            Ok(addresses) => resolved.extend(addresses.into_iter().map(|a| (peer, a))),
            Err(err) => tracing_unavailable(entry, &err),
        }
    }
    resolved
}

/// Kept as a function so this module does not take a logging dependency
/// on the shape of its caller. `tracing` is already in the tree.
fn tracing_unavailable(entry: &str, err: &str) {
    tracing::debug!(
        bootstrapper = entry,
        error = err,
        "could not resolve a DHT bootstrapper"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one string that keeps this network private.
    ///
    /// Two Kademlia instances interoperate only if their protocol names
    /// match, so if this ever reverted to the public name the node would
    /// rejoin the public IPFS DHT — disclosing which peer holds which
    /// trade attachment, and putting thousands of peers that do not speak
    /// this protocol into the swarm's single shared connection set, which
    /// gossip then broadcasts over.
    #[test]
    fn the_dht_protocol_is_this_networks_own_and_not_the_public_ipfs_one() {
        assert_eq!(OPENFIAT_KAD_PROTOCOL.as_ref(), "/openfiat/kad/1.0.0");
        assert_ne!(OPENFIAT_KAD_PROTOCOL.as_ref(), "/ipfs/kad/1.0.0");
    }

    /// A public bootstrapper is how the node ended up on the public DHT.
    /// The private DHT is seeded from `--entrypoint` peers instead, so
    /// this list stays empty — an entry here would quietly undo the
    /// separation the protocol name creates.
    #[test]
    fn there_are_no_public_bootstrappers() {
        assert!(
            BOOTSTRAPPERS.is_empty(),
            "a bootstrapper here dials this node onto somebody else's DHT"
        );
    }

    #[test]
    fn records_are_republished_before_the_network_expires_them() {
        assert!(PROVIDER_REPUBLISH_INTERVAL < PROVIDER_RECORD_TTL);
    }
}
