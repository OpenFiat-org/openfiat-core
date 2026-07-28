# openfiat-crypto

Ed25519 keypair generation/signing/verification (`Keypair`, `verify`) and
SHA-256 hashing (`hash::sha256`). Every signed event, registration, and
record across every spec in this workspace is authenticated with this
crate's primitives — it deliberately knows nothing about *what* it's
signing, only how.

## Depends on

- `openfiat-types` — `PublicKey`/`Signature` are the shared wire shapes
  this crate's keys and signatures serialize as.

## Used by

Every crate that signs or verifies something: `network`, `discovery`,
`gossip`, `snapshot`, `sessions`, `registry`, `identity`, `wallet`,
`reputation`, `trade`, `advertisements`, `reservations`, `settlement`,
`disputes`, `governance`, `notifications`, `oracles`, `risk`, `rpc`.
