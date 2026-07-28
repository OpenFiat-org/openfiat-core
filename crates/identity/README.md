# openfiat-identity

Account identity claims (OFS-5000): publish/verify/revoke a claim
(email, phone, Telegram, merchant name, ...) tied to a wallet, replicated
over gossip. `ClaimPublish` is self-consistency verified; `ClaimVerify`/
`ClaimRevoke` are verified against the claim's on-file wallet key. Real OTP
delivery/verification (the actual SMS/email/Telegram provider integration)
is out of scope here — this crate only defines the claim lifecycle; whether
a claim was actually verified out-of-band is the caller's (wallet app's)
responsibility.

Not to be confused with `openfiat-network`'s node identity (`PeerId`) — this
is *account* identity, one layer up.

**Spec:** OFS-5000 — OpenFiat Identity Claims Protocol

## Depends on

- `openfiat-gossip` — claim events travel as gossip events.
- `openfiat-network`, `openfiat-types`, `openfiat-crypto`,
  `openfiat-serialization`, `openfiat-storage` — shared transport, types,
  signing, and storage.

## Used by

- `openfiat-rpc` — exposes claim publish/lookup over JSON-RPC.
