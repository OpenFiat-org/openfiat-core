# openfiat-wallet

No OFS spec of its own — a shared `Wallet` (keypair + derived `PeerId`)
implementation for anything in this workspace that needs a wallet
identity: the signed-request auth primitive (`RequestEnvelope`/
`SignedRequest`/`verify_request`, mirroring a nonce/timestamp-scoped
authenticated request) and, later, Solana staking/governance instruction
builders that sign transactions with the same key.

Also loads/saves node identity as a Solana CLI-format `wallet.json`
(`solana_keyfile`) — the same 64-byte keypair file `solana-keygen new`
produces — so an operator can authenticate a node with a wallet they
already use for Solana tooling instead of managing a second identity.

## Depends on

- `openfiat-crypto` — the keypair a `Wallet` wraps.
- `openfiat-network` — `PeerId` derivation.
- `openfiat-types`, `openfiat-serialization` — shared types and wire
  encoding for `SignedRequest`.

## Used by

Nothing inside this workspace yet — `openfiat-apps/explorer/indexer`
(a separate repo) uses `solana_keyfile` to load its own node identity.
