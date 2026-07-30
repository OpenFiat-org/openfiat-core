# Running `openfiat-node` as a Windows Service

`openfiat-node.exe` is a plain console application — it doesn't register
itself with the Service Control Manager. On a Windows server, wrap it with
[NSSM](https://nssm.cc/) (the standard, well-established way to run an
arbitrary console executable as a real Windows Service, with automatic
restart and Event Log integration), the same pattern widely used for
Node.js/Java services that don't have native SCM support built in.

## Install

1. Download and extract [NSSM](https://nssm.cc/download); put `nssm.exe`
   somewhere on `PATH`.
2. Extract the `openfiat-node-windows-x86_64.zip` release asset to e.g.
   `C:\OpenFiat\openfiat-node.exe`.
3. Create a data directory, e.g. `C:\ProgramData\OpenFiat`.
4. Install the service:

   ```powershell
   nssm install OpenFiatNode "C:\OpenFiat\openfiat-node.exe"
   nssm set OpenFiatNode AppDirectory "C:\ProgramData\OpenFiat"
   nssm set OpenFiatNode AppParameters `
     "--ledger C:\ProgramData\OpenFiat " + `
     "--identity C:\ProgramData\OpenFiat\wallet.json " + `
     "--rpc-bind-address 0.0.0.0:7080 " + `
     "--gossip-bind-address /ip4/0.0.0.0/udp/4001/quic-v1"
   nssm set OpenFiatNode AppStdout "C:\ProgramData\OpenFiat\node.log"
   nssm set OpenFiatNode AppStderr "C:\ProgramData\OpenFiat\node.log"
   nssm set OpenFiatNode Start SERVICE_AUTO_START
   nssm start OpenFiatNode
   ```

   Add `--entrypoint` and `--solana-rpc-url` to `AppParameters` the same
   way if needed, and `--ipfs-api-url` to pin content and earn the full
   reward share. `openfiat-node --help` lists every flag; there is no
   environment-variable fallback and no config file, so `AppParameters` is
   the whole configuration surface.

5. NSSM sends the service a graceful `CTRL_SHUTDOWN_EVENT` on stop, which
   `openfiat-node` handles the same way it handles Ctrl+C — closing
   gossip connections cleanly rather than being killed outright.

## Manage

```powershell
nssm status OpenFiatNode
nssm restart OpenFiatNode
nssm stop OpenFiatNode
nssm remove OpenFiatNode confirm
```

## Firewall

Open the HTTP port (`7080` by default, TCP) and the gossip port (`4001`
by default, UDP) for inbound connections if this node needs to be
reachable from outside the host:

```powershell
New-NetFirewallRule -DisplayName "OpenFiat RPC" -Direction Inbound -Protocol TCP -LocalPort 7080 -Action Allow
New-NetFirewallRule -DisplayName "OpenFiat Gossip" -Direction Inbound -Protocol UDP -LocalPort 4001 -Action Allow
```
