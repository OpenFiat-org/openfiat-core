# The confidential trade channel (#100, #101)

Two things a P2P trade needs that this protocol did not have:

1. The seller has to hand the buyer real payment details — a bank
   account, a phone number, a reference. Those are private to the two
   parties.
2. The two parties need to talk while the trade runs, and if it goes to
   arbitration an arbitrator has to be able to read that conversation.

Both are the same shape — a confidential payload attached to one
settlement — so both are one crate, `openfiat-tradechannel`, and one
record type with a `kind` on it.

## The constraint

Every protocol event here is gossiped to every node and stored forever.
So "sealed" cannot mean "the read path checks who is asking": several
hundred machines already hold a copy, and an RPC gate protects none of
them. It has to mean encrypted, to a named key, at rest.

`openfiat-notifications` already solved exactly this for delivery
destinations (#98): a wallet's email address or phone number is put in an
`openfiat_crypto::SealedBox` addressed to the one gateway that will
deliver it, and gossiped as ciphertext. That mechanism is reused here
unchanged, and this document is mostly about the single place it did not
fit.

## Where a plain sealed box does not fit, and what was added

A sealed box addresses a recipient you know at the time you encrypt. For
payment details that is fine — the counterparty is known. For a
conversation that an arbitrator may later have to read, it is not: **the
arbitrator does not exist yet.** They are drawn when a dispute opens,
which is after every message has already been written, signed and
gossiped.

Three options were on the table.

**Seal to arbitrators up front.** Impossible. There is nobody to seal to.

**Escrow the key with the network** so an arbitrator can obtain it later.
Rejected outright: that is "sealed" in name while every node operator can
read every trade.

**Re-seal the messages to arbitrators when a dispute opens.** Workable
and rejected, for a reason that is worth stating because it is not the
obvious one. The cost objection is real but minor (O(messages) work, and
the disclosing party's client must still hold every plaintext). The
serious objection is integrity: re-sealing puts the *disclosing party* in
charge of re-encrypting the transcript. Nothing would stop them handing
the arbitrator a conversation that never happened, because the arbitrator
would only ever see bytes that party produced after the argument started.
The signatures on the originals would not help — they would not be what
the arbitrator was reading.

**What was built: one content key per trade, sealed per reader.**
Standard hybrid encryption. A client generates 32 random bytes for the
trade. Every entry — payment details and chat alike — is encrypted under
that key with ChaCha20-Poly1305. The key itself is distributed by
`openfiat_crypto::seal`, one small `KeyGrant` per reader, gossiped like
anything else.

Widening the audience is then one 32-byte grant, and — the point — the
arbitrator opens **the original ciphertexts**: authored, signed,
timestamped and replicated across the network before anyone knew there
would be a dispute. They read the history rather than a party's retelling
of it.

## Who a grant is addressed to, and why it is not the wallet key

Sealing to the recipient's Ed25519 wallet key was the obvious thing and
it was wrong, in a way that made the whole feature unusable rather than
merely awkward. `openfiat_crypto::open` needs the recipient's secret
scalar to complete the ECDH. A gateway has one. A node has one. **A
person with a browser wallet does not** — Solana wallets expose
`signMessage` and `signTransaction` and no key material at all, by design
and rightly. So every grant addressed to a real user was bytes that user
could never open, and two people with ordinary wallets could not exchange
payment details at all. Entries rendered as *sealed*, which was honest,
and the feature did not work.

A grant is now sealed to an X25519 **encryption key** the recipient
published as an OFS-5000 `ClaimType::EncryptionKey` claim. The client
derives that key from the wallet's own signature over one fixed,
domain-separated message (`openfiat_crypto::DERIVATION_MESSAGE`) and
publishes only the public half. `openfiat_crypto::seal_to_x25519` is the
same construction as `seal` with the Ed25519 → Montgomery conversion
removed; the `SealedBox` on the wire is unchanged, and `seal` stays for
the recipients that legitimately hold a signing key — notification
gateways, above all.

Three consequences, and the third is the cost:

- **A counterparty is addressable without a key exchange.** The claim is
  signed by the wallet it belongs to and a node refuses one whose
  `wallet` does not derive from its own `wallet_public_key`, so looking
  a key up by peer id is safe: nobody can publish an encryption key on
  somebody else's behalf.
- **A client recovers.** The key is a function of the wallet and nothing
  else, so the same wallet on a new device re-derives it and opens every
  grant ever addressed to it — including the self-grant, which is what
  that grant was always for. The channel key no longer has to survive in
  browser storage.
- **The derivation signature is the private key.** A site that persuades
  a user to sign that exact message can read every channel that wallet is
  party to, past and future. It cannot move funds — the message
  authorises nothing and is not a valid transaction — but there is no
  forward secrecy here (see below), so the exposure is total and
  permanent for the channels that already exist. Rotating the claim
  limits what is exposed *next* and undoes nothing.

Determinism is load-bearing: Ed25519 signing is deterministic by
construction (RFC 8032 §5.1.6 derives the per-signature nonce by hashing
the private prefix with the message, with no randomness), so the same
message yields the same signature and the same key. A wallet that
randomised its signatures anyway would strand its user. That must never
fail silently, so a client signs twice at enrolment and compares, and
afterwards compares each derived public key against the published claim
— see `openfiat-app`'s `lib/channel-identity.ts`.

**What was not done.** A node-issued identity key was the other
candidate: it needs no wallet signature and no claim, and it is key
escrow with better manners. Whoever issues the key can read every channel
sealed to it, so every node operator would become a reader of every
trade — the exact outcome the "escrow the key with the network" option
was rejected for above. That it would have been more convenient is not an
argument.

## What each observer can actually see

### A node operator, holding a full replica

- That a channel exists, and for which settlement.
- Every grant: who granted, to whom, when, with which key id. The sealed
  key is opaque to them.
- Every entry: which settlement, which party wrote it, its sequence
  number, its claimed timestamp, whether it is `PaymentDetails` or
  `Message`, and its padded ciphertext length.
- **Not** the payment details. **Not** one word of the conversation.

That metadata is real and unavoidable — a replicated log cannot carry an
entry without carrying the fact that it exists — so it is written down
rather than glossed over. Padding to 256-byte blocks removes the cheapest
inference from it (a "yes" and an account number are the same size on the
wire), but timing, ordering and authorship are visible and always will
be.

### An arbitrator

- Before a party grants them the key: exactly what a node operator sees,
  and nothing more. Joining a dispute does not by itself open anything.
- After a grant: **the entire channel**, including every message and
  every payment detail written before the dispute existed.

Disclosure is deliberately all-or-nothing. There is no way to hand over
three of five messages, because a curated transcript is an argument, not
evidence.

Either party can disclose, and disclosing hands over the counterparty's
half of the conversation too, with no consent step. That is unavoidable
and grants nothing new: a party can already read their whole channel and
photograph the screen. The same answer covers the sharper version — a
party arranging for an accomplice to join the dispute as an arbitrator
and granting to them. It buys the accomplice nothing the party could not
have forwarded by hand, and unlike forwarding it leaves a signed,
replicated record naming exactly who was let in.

Nobody can be compelled to disclose. What the protocol does instead is
make refusal *visible*: grants are public records, so `readers()` is a
complete and checkable answer to "who was let in", and an arbitrator
looking at a channel with no grant addressed to them can see that, and so
can everyone else. A party who withholds the conversation also cannot
cite it.

### A party

Everything, forever, including after the trade closes. A grant cannot be
revoked — the recipient already holds bytes that every node has a copy of
— and this crate does not pretend otherwise. A party who wants a fresh
audience starts a new channel key; entries from that point carry a new
key id.

## What is deliberately not provided

**Forward secrecy.** One long-lived key per trade, sealed under a
long-lived encryption key that is itself a function of the wallet.
Compromising either — the wallet's secret, or a signature over the
derivation message — exposes every channel that wallet was ever in. This is not a deferral: the requirement
is that a third party nobody can name yet must be able to read this
*later*, which is the precise opposite of forward secrecy. The two cannot
both be had, and the dispute requirement is the one this protocol exists
for.

**Verification that a grant contains the right key.** A sealed box is
opaque to everyone but its recipient — that is what makes it useful — so
a node cannot check that a granter sealed the real key. Sealing garbage
is self-defeating (it only prevents a reader the granter wanted), and the
public `key_id` makes even that detectable: a recipient hashes what they
opened and compares. Grants are keyed by `(recipient, granter)` so one
party cannot overwrite the other's honest grant with a broken one.

## Presence and typing indicators: scoped out, and why

#101 asked for presence and typing indicators. They are not here, and
they should not be.

A "typing" signal has a useful lifetime of about three seconds. The only
transport this protocol has is a gossip log that is replicated to every
node and kept forever. Putting the two together is wrong three times
over:

1. **Volume.** Typing is keystroke-rate. It would instantly become the
   highest-frequency event class in the network by orders of magnitude,
   and every byte of it is stored permanently by every node, in exchange
   for information that is stale before it finishes propagating.
2. **Permanence.** By the time a "user X is typing" event reaches a peer,
   it is already probably false. A log whose entries cannot be deleted is
   the worst possible home for a fact that is true for three seconds.
3. **Metadata.** This is the decisive one. Presence cannot be made
   confidential the way a message can, because the metadata *is* the
   content: who, on which trade, at which second. Encrypting it
   accomplishes nothing. What the network would accumulate is a permanent,
   public, second-by-second activity timeline for every wallet — when a
   trader sleeps, which timezone they are in, when they are alone with
   their phone. In a market where people meet to exchange cash, that is a
   physical-safety fact, and it is exactly the harvestable trail
   `openfiat-trade`'s counterparty view already refuses to create.

Presence needs a different transport: something ephemeral, direct, and
not replicated. Two shapes fit, and both are client/transport work rather
than protocol work:

- **A direct libp2p stream between the two peers.** Correct and fully
  peer-to-peer; presence reaches exactly the one person entitled to it
  and is stored nowhere. Needs the two clients to be able to establish a
  direct connection.
- **A node-local, in-memory, TTL'd presence cell** behind the RPC
  surface, never gossiped and never written to disk. Simple, but it only
  works when both parties happen to be talking to the same access node,
  which is a deployment accident rather than a protocol property.

Neither is built here. What would have been easy — and wrong — is
gossiping a `TypingStarted` event, so this is written down instead.

## Shape of the thing

- `KeyGrant` — `{ settlement, granter, recipient, role, key_id,
  sealed_key, granted_at }`. `role` (`Party` / `Arbitrator`) is derived
  by the node from the settlement and the dispute record, never claimed
  by the granter.
- `ChannelEntry` — `{ settlement, author, sequence, kind, payload,
  posted_at }`. Identified by `(settlement, author, sequence)` rather
  than a client-chosen id, so neither party can squat the other's slots;
  each side gets a contiguous run of numbers, which is also what lets a
  client notice a *missing* message instead of rendering a conversation
  with a hole in it.
- `payload` — `{ key_id, nonce, ciphertext }`. The AEAD's associated data
  binds the settlement, author, sequence and kind, so a ciphertext lifted
  into any other slot fails to open rather than appearing as something
  somebody else said.

Events ride OFS-2300 (settlement). No published OFS defines a
confidential trade channel, and minting a spec number for a document that
does not exist would be a claim this workspace cannot back; a channel
exists only for a settlement and dies with it, so it travels under that
settlement's spec with namespaced event names.

## RPC

- `sendTradeChannelKeyGrant` / `sendTradeChannelEntry` — already-signed
  payloads, same shape as every other `sendX`.
- `getMyTradeChannel` — wallet-proof gated, answers for a party to the
  settlement *or* any peer holding a grant (which is how a disclosed
  channel reaches an arbitrator). Gating a read of ciphertext is not
  confidentiality and is not claimed to be; it protects the metadata,
  which is the trade graph, for the reasons `methods::wallet_auth` gives.

## Client work this needs

All encryption is client-side. Nothing in a node holds a channel key or
has a code path that could use one. An SDK must:

1. Generate a 32-byte channel key per trade, and re-use the one already
   granted to it rather than minting a second (check `key_id`).
2. Seal it to the counterparty *and to itself* — a self-grant is how a
   client recovers the key on a new device — using the public keys the
   settlement record already carries.
3. Encrypt entries with ChaCha20-Poly1305 under that key, with the
   length-prefixed padded plaintext and the associated-data transcript
   described above.
4. On dispute, offer the user an explicit "share this conversation with
   the arbitrators" action that seals the key to each joined arbitrator's
   key from the dispute record — and make plain that it shares the whole
   channel, permanently, and cannot be taken back.
