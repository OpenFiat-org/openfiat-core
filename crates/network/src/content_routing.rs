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

/// The public IPFS DHT's protocol name.
///
/// `/ipfs/kad/1.0.0` and nothing else. libp2p-kad's own default is
/// `/ipfs/kad/1.0.0` too, but naming it here is what keeps it true when a
/// second Kademlia instance for OpenFiat peer discovery arrives — two kad
/// behaviours in one swarm work precisely because their protocol names
/// differ, and a default is not a name anyone chose.
pub const IPFS_KAD_PROTOCOL: StreamProtocol = StreamProtocol::new("/ipfs/kad/1.0.0");

/// Where a node joins the public IPFS DHT.
///
/// The libp2p Foundation's four bootstrappers, `/dnsaddr/` resolved to
/// the `/dns/` addresses they actually publish, plus the one historical
/// IPFS bootstrapper that is a literal IP. QUIC only for the hostnames:
/// this node's transport offers TCP as well, but a bootstrapper is dialled
/// once at startup and the QUIC address is the one every one of them
/// serves.
///
/// Five, not one, because bootstrapping is the single point where a node
/// depends on somebody else's host being up.
pub const BOOTSTRAPPERS: &[&str] = &[
    "/dns/sv15.bootstrap.libp2p.io/udp/4001/quic-v1/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN",
    "/dns/ny5.bootstrap.libp2p.io/udp/4001/quic-v1/p2p/QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa",
    "/dns/am6.bootstrap.libp2p.io/udp/4001/quic-v1/p2p/QmbLHAnMoJPWSCR5Zhtx6BHJX9KiKNN6tpvbUcqanj75Nb",
    "/dns/sg1.bootstrap.libp2p.io/udp/4001/quic-v1/p2p/QmcZf59bWwK5XFi76CZX8cbJ4BhTzzA3gU1ZjYZcYW3dwt",
    "/ip4/104.131.131.82/udp/4001/quic-v1/p2p/QmaCpDMGvV2BGHeYERUEnRQAwe3N8SzbUtfsmvsqQLuvuJ",
];

/// The DHT behaviour a node runs: a client of the public IPFS DHT.
pub fn behaviour(local_peer_id: Libp2pPeerId) -> kad::Behaviour<MemoryStore> {
    let mut config = kad::Config::new(IPFS_KAD_PROTOCOL);
    // A record is republished before the network expires it. IPFS drops a
    // provider record after 24 hours, and a node that only republished on
    // the hour it expired would be unfindable for whatever slice of an
    // hour the two clocks disagreed by.
    config.set_provider_publication_interval(Some(PROVIDER_REPUBLISH_INTERVAL));
    config.set_provider_record_ttl(Some(PROVIDER_RECORD_TTL));

    let mut behaviour =
        kad::Behaviour::with_config(local_peer_id, MemoryStore::new(local_peer_id), config);
    // See the module doc. This is the decision, not a default.
    behaviour.set_mode(Some(kad::Mode::Client));
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

    #[test]
    fn every_bootstrapper_is_a_real_address_naming_a_peer() {
        // The peer id is what makes an unauthenticated DNS lookup safe:
        // a hijacked record points at a host that cannot complete the
        // handshake. An entry without one would silently make this node
        // dial whoever DNS says.
        for entry in BOOTSTRAPPERS {
            let address: Multiaddr = entry.parse().expect(entry);
            assert!(
                crate::identity::peer_id_in_multiaddr(&address).is_some(),
                "{entry} would let DNS decide who this node talks to"
            );
        }
    }

    #[test]
    fn the_bootstrappers_are_distinct_hosts() {
        // Five entries all pointing at one host would be one bootstrapper
        // written five times, and bootstrapping is the one place a node
        // depends on somebody else being up.
        let peers: std::collections::HashSet<_> = BOOTSTRAPPERS
            .iter()
            .filter_map(|entry| {
                crate::identity::peer_id_in_multiaddr(&entry.parse::<Multiaddr>().unwrap())
            })
            .collect();
        assert_eq!(peers.len(), BOOTSTRAPPERS.len());
    }

    #[test]
    fn no_bootstrapper_needs_a_dnsaddr_resolver_this_workspace_does_not_have() {
        // The canonical list is written with `/dnsaddr/`, which is a TXT
        // indirection nothing here can follow. Resolving that chain by
        // hand once, at authoring time, is what makes these dialable —
        // and an entry that slipped back to `/dnsaddr/` would fail at
        // runtime, on a node, silently.
        for entry in BOOTSTRAPPERS {
            assert!(
                !entry.contains("/dnsaddr/"),
                "{entry} cannot be resolved by `resolve_dns_multiaddr`"
            );
        }
    }

    #[test]
    fn the_dht_protocol_is_the_public_ipfs_one() {
        // A different protocol name would be a private DHT that no
        // gateway queries — the node would publish records nobody reads
        // and look correct doing it.
        assert_eq!(IPFS_KAD_PROTOCOL.as_ref(), "/ipfs/kad/1.0.0");
    }

    #[test]
    fn records_are_republished_before_the_network_expires_them() {
        assert!(PROVIDER_REPUBLISH_INTERVAL < PROVIDER_RECORD_TTL);
    }
}
