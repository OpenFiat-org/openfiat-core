//! Deriving this node's own snapshot base URL from an address it has
//! learned it is reachable at.
//!
//! # Why this replaces a flag
//!
//! Announcing a snapshot used to require `--snapshot-public-url`, and
//! omitting it disabled snapshot production outright. The justification
//! was sound at the time: a node binds `0.0.0.0` and cannot see what a
//! NAT or a proxy makes of that, so the operator declared the public URL
//! by hand.
//!
//! It no longer holds. A running node learns where it is reachable from
//! two independent sources it already consumes — libp2p's `NewListenAddr`
//! reports one concrete address per interface once the wildcard bind is
//! expanded, and identify's `observed_addr` reports what a *peer* saw the
//! connection arrive from, which is the only thing that can see through
//! NAT. The exact fact the operator was asked to supply is now something
//! the node is told. So snapshot production is on by default and the flag
//! survives only as an override, for the node whose HTTP server is genuinely
//! reached on a hostname or port it has no way to observe (a reverse proxy).
//!
//! # Why a wrong guess here is cheap
//!
//! A derived URL is a hint about where bytes might be, never a statement
//! about what they are: `state_root` and `size_bytes` are checked before
//! anything is imported (see [`crate::location`]). The worst a bad
//! derivation costs a fetching node is one failed request before it tries
//! the next announced location. That asymmetry is what makes deriving
//! acceptable where guessing a *gossip* address would not be — a peer
//! cannot verify that a dial address is this node, but it can verify every
//! byte of a snapshot.

use crate::location::SnapshotLocation;
use openfiat_network::{Multiaddr, Protocol};
use std::net::{Ipv4Addr, Ipv6Addr};

/// How many derived locations an announcement carries.
///
/// A host with many interfaces (a cloud instance with IPv4, IPv6, a
/// container bridge and a VPN tap) would otherwise put every one of them
/// into a gossiped record that every node in the cluster stores forever.
/// Four is past the point where the ordering below has already put the
/// useful ones first.
const MAX_DERIVED_LOCATIONS: usize = 4;

/// How likely an address is to be reachable by a peer that is not on this
/// host, which is the order a fetching node should try them in.
///
/// Private addresses are kept rather than dropped, for the same reason
/// `openfiat_network::identity::is_dialable` keeps them: a docker-compose
/// cluster or a LAN reaches its peers only by private address, and that is
/// a real deployment. They just go last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Reach {
    Global,
    Private,
}

/// The host part of `address` as a URL authority, with how widely it can
/// be reached — or `None` if it names no host this node could serve on.
///
/// Loopback and the bind wildcards are excluded rather than ranked last.
/// Unlike a private address, they cannot be right for *any* peer: every
/// node in the cluster resolving `127.0.0.1` to itself is not a mirror,
/// it is a self-inflicted 404 on every node at once.
fn host(address: &Multiaddr) -> Option<(String, Reach)> {
    address.iter().find_map(|protocol| match protocol {
        Protocol::Ip4(ip) => {
            (!ip.is_loopback() && !ip.is_unspecified()).then(|| (ip.to_string(), reach4(ip)))
        }
        // Bracketed, because `http://::1:7080` is not a URL — the colons
        // of an IPv6 literal and the colon before a port are the same
        // character to every parser that will read this.
        Protocol::Ip6(ip) => {
            (!ip.is_loopback() && !ip.is_unspecified()).then(|| (format!("[{ip}]"), reach6(ip)))
        }
        // A name someone configured is a name someone can resolve; there
        // is nothing local to inspect that would say otherwise.
        Protocol::Dns(name) | Protocol::Dns4(name) | Protocol::Dns6(name) => {
            Some((name.to_string(), Reach::Global))
        }
        _ => None,
    })
}

fn reach4(ip: Ipv4Addr) -> Reach {
    let [a, b, ..] = ip.octets();
    // 100.64.0.0/10 — carrier-grade NAT. It looks routable and never is,
    // which is exactly the address a node should offer last rather than
    // first. Spelled out because `Ipv4Addr::is_shared` is unstable.
    let carrier_grade_nat = a == 100 && (64..128).contains(&b);
    if ip.is_private() || ip.is_link_local() || carrier_grade_nat {
        Reach::Private
    } else {
        Reach::Global
    }
}

fn reach6(ip: Ipv6Addr) -> Reach {
    // fc00::/7 (unique local) and fe80::/10 (link local). Spelled out
    // because `Ipv6Addr::is_unique_local` is still unstable, and a node
    // announcing a `fd00:` address as globally reachable sends every
    // peer outside its own network on a timeout.
    let first = ip.segments()[0];
    if (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80 {
        Reach::Private
    } else {
        Reach::Global
    }
}

/// The snapshot base URLs this node can announce, given the addresses it
/// has learned it is reachable at and the port its HTTP server answers on.
///
/// `addresses` is expected in the caller's own preference order
/// (operator-declared first — see `DiscoveryService::announced_addresses`);
/// that order is preserved within a reachability class, and globally
/// reachable hosts are lifted above private ones so a fetching node's
/// first attempt is its most likely to work.
///
/// The port comes from the node's own RPC bind, not from the multiaddr:
/// these addresses were learned about the *gossip* transport, and only the
/// host part of one carries over. Snapshots ride the RPC port (see
/// [`crate::serve`] for why they share a listener at all).
pub fn base_urls(addresses: &[Multiaddr], rpc_port: u16) -> Vec<SnapshotLocation> {
    let mut hosts: Vec<(String, Reach)> = Vec::new();
    for address in addresses {
        let Some((host, reach)) = host(address) else {
            continue;
        };
        if !hosts.iter().any(|(known, _)| known == &host) {
            hosts.push((host, reach));
        }
    }
    hosts.sort_by_key(|(_, reach)| *reach);
    hosts
        .into_iter()
        .filter_map(|(host, _)| SnapshotLocation::parse(format!("http://{host}:{rpc_port}")).ok())
        .take(MAX_DERIVED_LOCATIONS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addrs(raw: &[&str]) -> Vec<Multiaddr> {
        raw.iter().map(|a| a.parse().unwrap()).collect()
    }

    fn urls(raw: &[&str], port: u16) -> Vec<String> {
        base_urls(&addrs(raw), port)
            .iter()
            .map(|l| l.to_string())
            .collect()
    }

    #[test]
    fn a_bound_interface_address_becomes_a_base_url_on_the_rpc_port() {
        assert_eq!(
            urls(&["/ip4/203.0.113.9/udp/4001/quic-v1"], 7080),
            ["http://203.0.113.9:7080"]
        );
    }

    /// The whole point of the change: the address identify reported is
    /// enough, so nothing had to be configured.
    #[test]
    fn an_observed_address_behind_nat_is_enough_to_announce_from() {
        assert_eq!(
            urls(&["/ip4/198.51.100.4/udp/33421/quic-v1"], 7080),
            ["http://198.51.100.4:7080"]
        );
    }

    #[test]
    fn a_public_address_is_offered_before_a_private_one() {
        assert_eq!(
            urls(
                &[
                    "/ip4/10.0.0.4/udp/4001/quic-v1",
                    "/ip4/172.17.0.2/udp/4001/quic-v1",
                    "/ip4/203.0.113.9/udp/4001/quic-v1",
                ],
                7080
            ),
            [
                "http://203.0.113.9:7080",
                "http://10.0.0.4:7080",
                "http://172.17.0.2:7080",
            ]
        );
    }

    /// Announcing these would point every node in the cluster at itself.
    #[test]
    fn loopback_and_wildcard_addresses_never_become_locations() {
        assert!(
            urls(
                &[
                    "/ip4/127.0.0.1/udp/4001/quic-v1",
                    "/ip4/0.0.0.0/udp/4001/quic-v1",
                    "/ip6/::1/udp/4001/quic-v1",
                    "/ip6/::/udp/4001/quic-v1",
                ],
                7080
            )
            .is_empty()
        );
    }

    #[test]
    fn an_ipv6_literal_is_bracketed_so_the_port_is_still_a_port() {
        assert_eq!(
            urls(&["/ip6/2001:db8::7/udp/4001/quic-v1"], 7080),
            ["http://[2001:db8::7]:7080"]
        );
    }

    #[test]
    fn a_unique_local_ipv6_address_ranks_as_private() {
        assert_eq!(
            urls(
                &[
                    "/ip6/fd00::5/udp/4001/quic-v1",
                    "/ip6/2001:db8::7/udp/4001/quic-v1",
                ],
                7080
            ),
            ["http://[2001:db8::7]:7080", "http://[fd00::5]:7080"]
        );
    }

    #[test]
    fn a_dns_address_keeps_its_hostname() {
        assert_eq!(
            urls(&["/dns4/archive.example/udp/4001/quic-v1"], 7080),
            ["http://archive.example:7080"]
        );
    }

    /// The same host learned from a listen address and from a peer's
    /// observation is one location, not two.
    #[test]
    fn the_same_host_on_two_transports_is_announced_once() {
        assert_eq!(
            urls(
                &[
                    "/ip4/203.0.113.9/udp/4001/quic-v1",
                    "/ip4/203.0.113.9/tcp/4001",
                ],
                7080
            ),
            ["http://203.0.113.9:7080"]
        );
    }

    #[test]
    fn a_many_homed_host_does_not_announce_every_interface() {
        let many: Vec<String> = (1..=10)
            .map(|n| format!("/ip4/10.0.0.{n}/udp/4001/quic-v1"))
            .collect();
        let addresses: Vec<Multiaddr> = many.iter().map(|a| a.parse().unwrap()).collect();
        assert_eq!(base_urls(&addresses, 7080).len(), MAX_DERIVED_LOCATIONS);
    }
}
