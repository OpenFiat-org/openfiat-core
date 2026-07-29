# Running `openfiat-node` as a systemd service

```bash
sudo useradd --system --create-home --home-dir /var/lib/openfiat --shell /usr/sbin/nologin openfiat
sudo install -m 755 openfiat-node /usr/local/bin/openfiat-node
sudo mkdir -p /etc/openfiat
sudo install -m 640 -o openfiat -g openfiat node.env.example /etc/openfiat/node.env
sudo install -m 644 openfiat-node.service /etc/systemd/system/openfiat-node.service
sudo systemctl daemon-reload
sudo systemctl enable --now openfiat-node
```

Edit `/etc/openfiat/node.env` first (see `node.env.example` for what each
variable does) — in particular, never commit a real `CLI_SOLANA_RPC_URLS`
endpoint/API key anywhere; it belongs only in that file on the server.

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

## Firewall

Open the HTTP port (`7080` by default, TCP) and the gossip port (`4001`
by default, UDP) if this node needs to be reachable from outside the
host, e.g. with `ufw`:

```bash
sudo ufw allow 7080/tcp
sudo ufw allow 4001/udp
```
