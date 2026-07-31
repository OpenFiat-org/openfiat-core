//! Bridges `openfiat_crypto`'s Ed25519 keypair into libp2p's identity types.
//!
//! OFNP §6 specifies one Node Identity — public key, private key, Peer ID —
//! reused for both the Noise transport handshake and higher-level protocol
//! message signing. Rather than maintaining two separate keypairs, this
//! module derives libp2p's identity directly from the same 32-byte Ed25519
//! seed `openfiat_crypto::Keypair` already holds, so a node's transport
//! identity and its OFNP-level signing identity are always the same key.

use libp2p::identity::{
    DecodingError, Keypair as Libp2pKeypair, PeerId as Libp2pPeerId, PublicKey as Libp2pPublicKey,
    ed25519,
};
use openfiat_crypto::Keypair;
use openfiat_types::{PeerId, PublicKey};

/// Derive libp2p's identity keypair from an `openfiat_crypto::Keypair`.
pub fn to_libp2p_keypair(keypair: &Keypair) -> Libp2pKeypair {
    let mut seed = keypair.seed();
    let secret = ed25519::SecretKey::try_from_bytes(&mut seed)
        .expect("a 32-byte seed is always a valid Ed25519 secret key");
    Libp2pKeypair::from(ed25519::Keypair::from(secret))
}

/// The `openfiat_types::PeerId` deterministically derived from a libp2p
/// identity keypair's public key (OFNP §6), using libp2p's own standard
/// derivation (multihash-wrapped public key encoding).
pub fn peer_id(libp2p_keypair: &Libp2pKeypair) -> PeerId {
    from_libp2p_peer_id(Libp2pPeerId::from(libp2p_keypair.public()))
}

/// Wrap an already-derived libp2p `PeerId` (e.g. from `Swarm::local_peer_id`
/// or a connection event) as the shared `openfiat_types::PeerId`.
pub fn from_libp2p_peer_id(id: Libp2pPeerId) -> PeerId {
    PeerId::from_bytes(id.to_bytes())
}

/// The `12D3Koo…` spelling of a protocol peer id.
///
/// `openfiat_types::PeerId` holds the libp2p peer id's own bytes and now
/// renders as base58 in JSON itself, so this exists for the places that
/// hold a `PeerId` and want the `12D3Koo…` string directly — a log line,
/// a multiaddr's `/p2p/` segment — without going through serde.
///
/// `None` for bytes that are not a peer id at all. Discovery caches a
/// placeholder record for a peer it has connected to but not yet learned
/// about, so a caller must be able to render an incomplete one without
/// asserting it is well-formed.
pub fn readable_peer_id(id: &PeerId) -> Option<String> {
    Libp2pPeerId::from_bytes(id.as_bytes())
        .ok()
        .map(|id| id.to_string())
}

/// Derive the `PeerId` a bare `openfiat_types::PublicKey` claims, without
/// needing the corresponding private key.
///
/// Used to check a discovered peer's self-consistency (OFS-1100 §10, §25):
/// an advertisement's stated Peer ID must actually be the one its stated
/// public key derives to, or it's rejected as peer poisoning.
pub fn peer_id_from_public_key(public_key: &PublicKey) -> Result<PeerId, DecodingError> {
    let ed25519_key = ed25519::PublicKey::try_from_bytes(public_key.as_bytes())?;
    let libp2p_key = Libp2pPublicKey::from(ed25519_key);
    Ok(from_libp2p_peer_id(Libp2pPeerId::from_public_key(
        &libp2p_key,
    )))
}

/// Recover the public key a freshly-connected peer's `PeerId` embeds,
/// with no wire round-trip needed. Sound specifically because this
/// workspace's node identity is always Ed25519 (OFNP §6): the libp2p
/// peer-id spec mandates the size-inline "identity" multihash (rather
/// than a one-way hash) whenever the protobuf-encoded public key is
/// under 42 bytes, which an Ed25519 key always is — so decoding it back
/// out is a documented guarantee here, not a coincidence to rely on
/// loosely. Used to auto-populate `GossipService`'s `peer_keys` map on
/// connection (see `crates/gossip/src/service.rs::handle`), since two
/// independently-started nodes have no other shared advance knowledge of
/// each other's signing key.
pub fn public_key_from_peer_id(id: Libp2pPeerId) -> Option<PublicKey> {
    let multihash = multihash::Multihash::<64>::from_bytes(&id.to_bytes()).ok()?;
    let libp2p_key = Libp2pPublicKey::try_decode_protobuf(multihash.digest()).ok()?;
    let ed25519_key = libp2p_key.try_into_ed25519().ok()?;
    Some(PublicKey::from_bytes(ed25519_key.to_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_derives_the_same_peer_id() {
        let keypair = Keypair::from_seed([9u8; 32]);
        let a = peer_id(&to_libp2p_keypair(&keypair));
        let b = peer_id(&to_libp2p_keypair(&keypair));
        assert_eq!(a, b);
    }

    #[test]
    fn different_seeds_derive_different_peer_ids() {
        let a = peer_id(&to_libp2p_keypair(&Keypair::from_seed([1u8; 32])));
        let b = peer_id(&to_libp2p_keypair(&Keypair::from_seed([2u8; 32])));
        assert_ne!(a, b);
    }

    #[test]
    fn public_key_from_peer_id_recovers_the_originating_keypairs_public_key() {
        let keypair = Keypair::from_seed([7u8; 32]);
        let libp2p_peer_id = Libp2pPeerId::from(to_libp2p_keypair(&keypair).public());
        let recovered = public_key_from_peer_id(libp2p_peer_id).unwrap();
        assert_eq!(recovered, keypair.public_key());
    }

    #[test]
    fn peer_id_from_public_key_matches_the_keypair_derived_one() {
        let keypair = Keypair::from_seed([3u8; 32]);
        let from_keypair = peer_id(&to_libp2p_keypair(&keypair));
        let from_public_key = peer_id_from_public_key(&keypair.public_key()).unwrap();
        assert_eq!(from_keypair, from_public_key);
    }
}

/// Whether a multiaddr is one another peer could actually dial.
///
/// Excludes the bind wildcards `0.0.0.0` and `::`, which mean "every
/// interface on this host" to a listening socket and nothing at all to a
/// dialing one. A node that announces, logs, or discovers its own bind
/// address hands out an address that can never connect, and — because it
/// looks like a real address — the failure surfaces as an unexplained
/// dial error on the far side rather than as anything wrong locally.
///
/// Loopback and private ranges pass deliberately. Processes on one host
/// reach each other over `127.0.0.1` perfectly well, and a docker-compose
/// cluster or a LAN reaches its peers only by private address; a
/// single-host dev cluster is a real deployment, not a test artifact. An
/// operator whose private address is useless to outsiders is the one who
/// knows it, and says so explicitly.
pub fn is_dialable(address: &libp2p::Multiaddr) -> bool {
    let text = address.to_string();
    !["/ip4/0.0.0.0/", "/ip6/::/"]
        .iter()
        .any(|wildcard| text.starts_with(wildcard))
}

#[cfg(test)]
mod dialable_tests {
    use super::is_dialable;

    #[test]
    fn the_bind_wildcard_is_never_dialable() {
        // The default --gossip-bind-address. Announcing or printing it
        // gives a peer an address that cannot connect.
        for wildcard in [
            "/ip4/0.0.0.0/udp/4001/quic-v1",
            "/ip6/::/udp/4001/quic-v1",
            "/ip4/0.0.0.0/tcp/0",
        ] {
            assert!(!is_dialable(&wildcard.parse().unwrap()), "{wildcard}");
        }
    }

    #[test]
    fn loopback_and_private_addresses_are_dialable() {
        for usable in [
            "/ip4/127.0.0.1/udp/4001/quic-v1",
            "/ip4/10.0.0.4/udp/4001/quic-v1",
            "/ip4/192.168.1.7/udp/4001/quic-v1",
            "/ip4/203.0.113.9/udp/4001/quic-v1",
        ] {
            assert!(is_dialable(&usable.parse().unwrap()), "{usable}");
        }
    }
}

/// Resolves a `/dns4/`, `/dns6/` or `/dns/` multiaddr into one address per
/// IP the hostname points at, leaving every other component — including
/// `/p2p/<peer id>` — untouched.
///
/// # Why this crate resolves DNS instead of libp2p
///
/// libp2p has a `dns` transport feature and this workspace deliberately
/// does not enable it: it pulls in `hickory-proto`, which has carried
/// unresolved advisories (see `docs/architecture.md`). The consequence
/// was that entrypoints had to be raw IP multiaddrs, which is a genuinely
/// worse operator experience — an IP that changes silently strands
/// everyone holding it, and `/ip4/84.32.223.111/udp/4001/quic-v1/p2p/12D3…`
/// is not something anyone can be asked to type or verify.
///
/// Resolving here uses the operating system's own resolver through
/// `std::net`, which every platform already has, so a hostname works
/// without adding a DNS implementation to the dependency tree.
///
/// # The `/p2p/` component is preserved, and that is the security of it
///
/// DNS is not authenticated. A hijacked record points a node at an
/// attacker's host — and because the peer id survives resolution, the
/// connection to that host fails the libp2p handshake unless the attacker
/// also holds the entrypoint's private key. Without a `/p2p/` component
/// there is nothing to check, and the attacker becomes your only peer:
/// they cannot forge events, since every event carries its origin's own
/// signature, but they can decide which ones you see. Keep the peer id.
/// Whether this host can reach the global IPv6 internet.
///
/// # Why this is asked at all
///
/// A `/dns/` name resolves to every address the host has records for, of
/// both families. On an IPv4-only host — which most VPS instances are
/// unless the operator asked otherwise — every IPv6 result is an address
/// the kernel has no route to, and QUIC does not fail those gracefully:
/// it fails each send with `NetworkUnreachable` and retries on the DHT's
/// own republish interval. The observed result was one `WARN` per
/// bootstrapper every five minutes, indefinitely, on a node that was
/// otherwise working perfectly — it had already joined the DHT over IPv4
/// and was announcing content. Noise that never stops trains an operator
/// to ignore the log, which is worse than the original problem.
///
/// # How it is answered
///
/// By `connect`ing a UDP socket to a global address. `connect` on a UDP
/// socket transmits nothing: it performs a route lookup and records a
/// default destination. So this costs one syscall, sends no packet,
/// contacts nobody, and returns exactly the error the transport would
/// have hit. The address used is documentation-range (RFC 3849) so that
/// even a misreading of this code cannot turn it into a probe of somebody
/// else's host.
///
/// Answered once and cached: a machine does not usually gain a default
/// route while a process runs, and re-probing per resolution would put a
/// syscall in a loop for an answer that does not change. A node that
/// *does* gain IPv6 picks it up on restart.
fn host_has_ipv6_route() -> bool {
    use std::sync::OnceLock;
    static HAS_ROUTE: OnceLock<bool> = OnceLock::new();

    *HAS_ROUTE.get_or_init(|| {
        let Ok(socket) = std::net::UdpSocket::bind("[::]:0") else {
            return false;
        };
        // 2001:db8::/32 — reserved for documentation, routed by nobody.
        let reachable = socket.connect("[2001:db8::1]:443").is_ok();
        if !reachable {
            tracing::info!(
                "no IPv6 route on this host: IPv6 addresses will be skipped when \
                 resolving hostnames, since dialling them produces an unreachable \
                 error on every attempt"
            );
        }
        reachable
    })
}

pub fn resolve_dns_multiaddr(
    address: &libp2p::Multiaddr,
) -> Result<Vec<libp2p::Multiaddr>, String> {
    use libp2p::multiaddr::Protocol;

    let mut components = address.iter().peekable();
    let Some(first) = components.next() else {
        return Ok(vec![address.clone()]);
    };

    let (host, want_v4, want_v6) = match first {
        Protocol::Dns4(name) => (name.to_string(), true, false),
        Protocol::Dns6(name) => (name.to_string(), false, true),
        Protocol::Dns(name) => (name.to_string(), true, true),
        // Not a hostname: nothing to resolve, hand it back unchanged.
        _ => return Ok(vec![address.clone()]),
    };

    // The port is irrelevant to resolution but `to_socket_addrs` demands
    // one, so a placeholder is used and the real components are re-appended
    // from the original address below.
    let resolved: Vec<std::net::IpAddr> =
        std::net::ToSocketAddrs::to_socket_addrs(&(host.as_str(), 0u16))
            .map_err(|e| format!("could not resolve {host}: {e}"))?
            .map(|socket| socket.ip())
            .filter(|ip| match ip {
                std::net::IpAddr::V4(_) => want_v4,
                // A `/dns/` name resolves to both families, and dialling
                // an address family this host has no route to is not a
                // slow failure — it is an immediate `NetworkUnreachable`
                // on every send, retried forever, once per bootstrapper.
                std::net::IpAddr::V6(_) => want_v6 && host_has_ipv6_route(),
            })
            .collect();

    if resolved.is_empty() {
        return Err(format!("{host} resolved to no usable address"));
    }

    let rest: Vec<Protocol> = address.iter().skip(1).collect();
    let mut out = Vec::with_capacity(resolved.len());
    for ip in resolved {
        let mut resolved_addr = libp2p::Multiaddr::empty();
        resolved_addr.push(match ip {
            std::net::IpAddr::V4(v4) => Protocol::Ip4(v4),
            std::net::IpAddr::V6(v6) => Protocol::Ip6(v6),
        });
        for component in &rest {
            resolved_addr.push(component.clone());
        }
        if !out.contains(&resolved_addr) {
            out.push(resolved_addr);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod dns_tests {
    use super::{host_has_ipv6_route, resolve_dns_multiaddr};

    /// The probe must report the same thing the transport would discover,
    /// because reporting anything else either reintroduces the unreachable
    /// dials or silently drops addresses that would have worked.
    ///
    /// Deliberately compares against a fresh, independent `connect` rather
    /// than a hardcoded expectation: whether this machine has IPv6 is a
    /// property of wherever the tests happen to run, and asserting either
    /// answer would make this test a report on the CI runner.
    #[test]
    fn the_ipv6_probe_agrees_with_what_a_dial_would_find() {
        let independent = std::net::UdpSocket::bind("[::]:0")
            .and_then(|socket| socket.connect("[2001:db8::1]:443"))
            .is_ok();
        assert_eq!(host_has_ipv6_route(), independent);
    }

    /// On a host with no IPv6 route, resolution must not hand back an IPv6
    /// address — that is the whole point. Vacuous where IPv6 works, which
    /// is stated rather than hidden.
    #[test]
    fn no_ipv6_address_survives_resolution_without_a_route() {
        if host_has_ipv6_route() {
            return;
        }
        let addr: libp2p::Multiaddr = "/dns/localhost/udp/4001/quic-v1".parse().unwrap();
        for resolved in resolve_dns_multiaddr(&addr).unwrap_or_default() {
            assert!(
                !resolved
                    .iter()
                    .any(|c| matches!(c, libp2p::multiaddr::Protocol::Ip6(_))),
                "resolution produced {resolved}, which this host cannot route to"
            );
        }
    }

    #[test]
    fn an_ip_multiaddr_is_returned_unchanged() {
        let addr: libp2p::Multiaddr = "/ip4/84.32.223.111/udp/4001/quic-v1".parse().unwrap();
        assert_eq!(resolve_dns_multiaddr(&addr).unwrap(), vec![addr]);
    }

    #[test]
    fn localhost_resolves_and_keeps_every_other_component() {
        // `localhost` is the one hostname guaranteed resolvable without a
        // network, so this asserts the rewriting rather than the DNS.
        let addr: libp2p::Multiaddr =
            "/dns4/localhost/udp/4001/quic-v1/p2p/12D3KooWD1Znm1N35pmNRpB5zzToghqTU2yrPzWCDDLVaekg6628"
                .parse()
                .unwrap();
        let resolved = resolve_dns_multiaddr(&addr).unwrap();

        assert!(!resolved.is_empty());
        for one in &resolved {
            let text = one.to_string();
            assert!(text.starts_with("/ip4/"), "{text}");
            assert!(text.contains("/udp/4001/quic-v1"), "{text}");
            assert!(
                text.contains("/p2p/12D3KooWD1Znm1N35pmNRpB5zzToghqTU2yrPzWCDDLVaekg6628"),
                "the peer id is what stops a hijacked DNS record \
                 substituting a different node: {text}"
            );
        }
    }

    #[test]
    fn a_hostname_that_does_not_exist_is_an_error_not_a_silent_skip() {
        // Silently dropping it would leave a node with no entrypoints and
        // no explanation, looking healthy and talking to nobody.
        let addr: libp2p::Multiaddr = "/dns4/nonexistent.invalid/udp/4001/quic-v1"
            .parse()
            .unwrap();
        assert!(resolve_dns_multiaddr(&addr).is_err());
    }
}
