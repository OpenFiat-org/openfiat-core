# Running `openfiat-node` as a systemd service

```bash
sudo useradd --system --create-home --home-dir /var/lib/openfiat --shell /usr/sbin/nologin openfiat
sudo install -m 755 openfiat-node /usr/local/bin/openfiat-node
sudo install -m 644 openfiat-node.service /etc/systemd/system/openfiat-node.service
sudo systemctl daemon-reload
sudo systemctl enable --now openfiat-node
```

## Configuration is the unit file, and only the unit file

Every setting is a flag on `ExecStart`. There is no `node.env`, no
`EnvironmentFile` and no config file — deliberately. With two sources a
node's real configuration becomes a function of the invocation *and* the
ambient environment, and "why does this node behave differently from the
identical one beside it" turns into archaeology across shell profiles and
unit files.

So editing the service means editing the unit:

```bash
sudo systemctl edit --full openfiat-node   # change flags
sudo systemctl restart openfiat-node
systemctl cat openfiat-node                # exactly what a running node was given
```

`openfiat-node --help` is the whole surface.

A real Solana RPC endpoint or API key belongs in this unit file on the
server and nowhere that is version controlled.

## Pinning content (optional, and it pays)

Content serving is on by default and needs no flag or daemon. Pass
`--no-content-serving` to turn it off, at the cost of the retrievability
share of rewards.
The node then pins the content that protocol records reference and can
answer another node's retrievability challenge, which earns the full
reward share; without it the node stores nothing and earns a reduced share
(OFS-4100 §9.2). See [`docs/getting-started.md`](../../docs/getting-started.md)
§5.

## Two things that will bite

**`AF_NETLINK` is required, not optional.** Binding a wildcard address
makes libp2p enumerate the host's interfaces, and that goes over a netlink
socket. Remove it from `RestrictAddressFamilies` and the QUIC listener
fails with a bare "Internal" error and the gossip actor panics — while the
HTTP thread survives, so systemd reports the unit active and the node
looks healthy from outside while serving nothing.

**Back up `/var/lib/openfiat/wallet.json`.** It is this node's identity and
it owns the node's stake. Lose it and any staked OPEN is stranded with no
way to unbond.

## Manage

```bash
systemctl status openfiat-node
journalctl -u openfiat-node -f
sudo systemctl restart openfiat-node
sudo systemctl stop openfiat-node
```

`openfiat-node` handles `SIGTERM` (what `systemctl stop` sends) by
shutting down gracefully rather than being killed outright — see
`crates/cli`'s `shutdown_signal`.

## Finding your node's addresses

The node logs its identity and, once listening, every address it is
actually reachable at:

```
INFO openfiat_node: starting … address=RK5Yejk… peer_id=12D3KooWAEgF…
INFO openfiat_rpc::actor: reachable at a new address …
     entrypoint=/ip4/203.0.113.9/udp/4001/quic-v1/p2p/12D3KooWAEgF…
```

`address` is the Solana address holding the node's stake. Each
`entrypoint` line is what another operator passes to their own
`--entrypoint`; pick the one routable from where they are.

## Firewall

Open the HTTP port (`7080` by default, TCP) and the gossip port (`4001`
by default, UDP) if this node needs to be reachable from outside the
host, e.g. with `ufw`:

```bash
sudo ufw allow 7080/tcp
sudo ufw allow 4001/udp
```
