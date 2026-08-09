# What a dishonest node can do

Anyone can run an OpenFiat node. There is no application, no allowlist and
no reputation gate on joining the gossip mesh, and there is not going to
be one — an open protocol whose safety rests on keeping bad operators out
is a permissioned protocol that has not admitted it yet.

So the question this document answers is not "how do we detect a liar".
It is: **when a node lies, replays, withholds, floods or fabricates, what
does it get, and who pays for it?** Where the answer was "something", the
code changed. Where the answer is "nothing, but you have to check for
yourself", that is written down here rather than left for a client author
to discover.

Everything below is stated at the level of what an attacker *can still
do*. A mitigation with no adversarial test behind it is a claim, not a
property, so each one names the test that performs the attack and asserts
it fails — see `crates/gossip/tests/adversarial.rs`.

---

## 1. Identity: what a node cannot forge

A `PeerId` in this workspace is a libp2p Ed25519 peer id, and such an id
carries the public key inline rather than hashing it away
(`openfiat_network::identity::public_key_from_peer_id`). The origin of an
event and the key that must have signed it are therefore *the same fact*,
derived, never asserted and never registered.

**A node cannot originate an event attributed to another identity.** Not
because it is refused permission to, but because it does not have the key,
and the origin field is the key.

This also settles a question that looks alarming and is not:
`openfiat_rpc::dispatch::originate` puts events on the wire on behalf of
wallets that are not this node. That is correct and safe. The gossip
envelope's origin is a **transport** fact — "this node relayed this" — and
the *authority* to say the thing lives in the inner payload, which carries
its own signature by the wallet that authored it and is verified by the
registry that applies it (`SignedAdvertisementCreate::verify`,
`SignedOraclePublish::verify`, and so on for every domain). A node
originating an event it did not author gains nothing: it can already relay
one.

The role check in `openfiat_gossip::authorization` is likewise about
*this* node's standing to originate, and is applied at origination only.
Nothing stops a remote node emitting an envelope typed `FXPriceUpdated`
without holding `OracleProvider` — and nothing needs to, because the
oracle index refuses the payload unless the *inner signer* is a registered
market-data provider (`OracleIndex::apply_publish`). Authorization is
enforced where the state changes, not where the bytes arrive.

---

## 2. What was closed

Each of these was reachable by any peer that could open a connection.

### The event id was a field the sender filled in

`EventId` is the dedup key and, per OGP §5, a deterministic function of
the event. It was computed that way at origination and **never checked on
arrival**. The signature covers everything except the id, so anyone
relaying a genuinely signed event could mint unlimited *distinct* copies
of it by varying only that field: each copy verified, each was a new row
in every peer's dedup store, each was handed to every domain handler, and
each was re-forwarded. One signature became unbounded traffic and
unbounded storage, and none of it looked like a replay, because a replay
is something the store recognises *by id*.

`validate` now recomputes the id and refuses a mismatch
(`GossipError::EventIdMismatch`, `event_id::matches`).
Test: `one_signature_cannot_be_ground_into_many_events_by_rewriting_the_id`
pushes sixty-four rewritten ids over one signature across a real
connection and asserts the receiver ends up with one event.

### A far-future timestamp was a permanent row

The event log is pruned by timestamp and by nothing else. An event stamped
in the year 3000 is older than no cutoff that will ever be computed, so it
survives every sweep forever. Events more than `MAX_CLOCK_SKEW_MILLIS`
(five minutes) ahead of this node's clock are now refused. A clock that is
merely wrong is still tolerated — that cost is bounded on purpose, and
tested (`a_slightly_fast_clock_is_still_believed`).

### TTL was a number from a stranger

`ttl` is the one envelope field the protocol expects to change in flight,
so it cannot be signed, so any relay can write anything into it. It is now
clamped to `MAX_TTL` on receipt.

Clamped, **not** rejected, and the direction is the whole point: a node
that refused an over-budget TTL would hand every relay a censorship
button — raise the field on someone else's signed event and watch the rest
of the network throw it away. Clamping costs the liar nothing to attempt
and gains them nothing either. The test asserts both halves
(`an_inflated_hop_budget_is_cut_back_rather_than_used_to_censor`).

### Recovery was a ~25,000x amplifier

A recovery request is a few dozen bytes. Its answer is as much of the
event log as fits in an envelope. Answering every request as it arrived
made any connected peer able to aim this node's whole log at itself, in a
loop, for free. The honest protocol asks exactly once, on connect, so that
is what is served; further asks on the same connection are answered with
an empty response. Re-arming costs an attacker a real reconnection,
handshake included, rather than nothing.

### Recovery had silently stopped working at all

Found while testing the above, and worse than the attack. `MAX_ENVELOPE_BYTES`
is a hard 1 MiB and the codec refuses to *write* anything larger, so a node
whose log had passed a megabyte — every node that has been up a day — built
the entire log, failed to encode it, and sent **nothing**. Not a truncated
answer: no answer. Every honest node was withholding by accident. Responses
are now filled to a byte budget, oldest first, so the requester makes
contiguous progress through its gap.

### Unanswered requests leaked stream slots

The transport is request-response: an inbound request occupies a stream
slot until it is answered or times out. This codebase already hit that
once through the push path (`MESSAGE_TYPE_PUSH_ACK` exists for it), but a
recovery request with an undecodable payload, or an envelope with a
message type this node does not implement, still dropped the channel.
Sending garbage was a cheaper flood than sending anything real. Both are
now answered.

### Address claims grew without bound

identify's `observed_addr` is free text on the far side of the connection.
One peer reporting a fresh well-formed address on every reconnection grew
`reachable` and `observed_by` for as long as it cared to. Both are capped
(`MAX_REACHABLE_ADDRESSES`, and a per-address reporter cap), which costs
nothing real: the number of true answers is the number of interfaces the
host has.

### A relayed event from two hops away did not validate

Not an attack — a bug the attacks uncovered, and the most damaging thing
here. Signing keys were cached from `ConnectionEstablished`, so a node
could verify its direct peers and *nobody else*. An event relayed two hops
names an origin the receiver has never connected to, so its key was
missing and the event was rejected as `InvalidSignature`. Epidemic
propagation past one hop did not work outside the test harness, and every
test in the workspace hid it by registering the whole cluster's keys by
hand. Keys are now derived from the origin's own `PeerId`, which is both
the fix and strictly safer — a derived binding cannot be told a different
key for an identity, and a map fed by remote input cannot grow without
bound if nothing remote feeds it.

### A flooding neighbour paid nothing for our forward budget

A directly connected peer could relay well-formed events as fast as its
link allowed, and each cost us a store write and a forward before anything
bounded it. There is now a per-**peer** relay credit
(`GOSSIP_PEER_CREDIT_CAPACITY`, refilled at `GOSSIP_PEER_CREDIT_REFILL_PER_SEC`):
a token bucket keyed on the *transport* peer that delivered the envelope —
never on the envelope's signed origin — checked before the signature
verify, so a flooder is turned away before it can force the expensive op.
An honest burst inside the bucket passes untouched; a peer sustaining more
than the refill has its excess dropped, and a peer past a drop threshold is
disconnected. This is explicitly **not** the per-origin limit §4 rejects:
it bounds one *link*, not one *identity*, so key rotation does not evade it
and an honest bursty origin does not trip it. See
`crates/gossip/tests/adversarial.rs`.

### The public RPC was unmetered

The JSON-RPC surface (`/rpc`, `/ws`) accepted requests from any client as
fast as they came. It is now rate-limited per client IP
(`RPC_RATE_BURST` / `RPC_RATE_REFILL_PER_SEC`, keyed on the socket peer, not
a spoofable header); `/health` and `/metrics` are exempt so monitoring is
never throttled. Behind a reverse proxy, honouring `X-Forwarded-For` is a
follow-up — v1 meters the socket peer.

### Connections grew without bound

The swarm accepted connections without a ceiling (noted below as belonging
in `openfiat_network` — now done). `crates/network`'s combined behaviour
carries a libp2p `connection_limits` guard
(`NETWORK_MAX_ESTABLISHED` / `NETWORK_MAX_ESTABLISHED_INCOMING`).

### One wallet's identity claims grew without bound

`apply_publish` accepted unbounded claims from a single wallet. A wallet is
now capped at `MAX_CLAIMS_PER_WALLET` **live** claims (non-revoked,
non-expired against the node's own clock, non-superseded). A supersede is
exempt only when it names a claim already in that wallet's live set, so a
supersede of a fake/foreign/dead id cannot buy an exemption, and the
liveness count uses trusted wall-clock time rather than the publisher's
self-reported timestamp — both were bypasses caught in review. Dead claims
are reclaimed by `prune` past the one-week retention the event log uses.

---

## 3. What you must not trust a node about

This is the part that matters to anyone writing a client.

**A node's RPC answers are unverifiable.** `getAdvertisements`,
`getReservation`, `getSettlement`, `getReputation` and every other read
return *derived views* — the record as that node's replica holds it, plus
things it computed. They do not carry the originating wallet's signature,
so nothing in the response ties it to anyone. A node can invent an
advertisement, alter a price, omit a settlement, or answer two clients
differently in the same second, and no amount of inspecting the response
will tell you. Two things follow:

- **Read from more than one node if the answer matters.** Disagreement is
  detectable even though dishonesty is not; agreement across unrelated
  operators is the only signal available at the RPC layer.
- **Do not treat an RPC read as a fact about the network.** Treat it as
  that node's claim about its replica.

The one class of statement you can check for yourself is anything settled
on Solana. Escrow state, escrow release and dispute resolution execute in
the on-chain programs pinned in `openfiat_chain::PROGRAM_IDS`, against
transactions your own wallet signed. A node relays those bytes and cannot
alter them; if it drops them instead, the transaction simply never
confirms and you can see that from any Solana RPC. **Money is verifiable
independently of the node. The order book is not.**

Correspondingly, a node **cannot**:

- change the terms of an advertisement, reservation or settlement without
  invalidating the inner signature the applying registry checks;
- make a trade settle, or unsettle, without the on-chain program agreeing;
- attribute anything to a wallet whose key it does not hold.

### The bootstrap assumption

A node with no history of its own takes its first snapshot from a pinned
key (`crates/snapshot/src/trust.rs`). That is weak subjectivity and is
documented there as a trust assumption rather than dressed up as
trustlessness. It applies once, to a node with nothing to lose, and the
alternative is not trustless bootstrap but "believe whoever answers
first".

---

## 4. Left open on purpose

### Withholding is not detectable, and mostly does not matter

A node can serve some records and not others — to gossip peers, to RPC
clients, or to one client and not another. There is no way to prove the
difference between "withholding" and "has not got it yet", short of a
consensus about what the complete set is, which this protocol deliberately
does not have.

It mostly does not matter because gossip is multi-path: an event reaching
you depends on *some* path existing, not on any particular node
cooperating, and a node that withholds from its peers removes itself from
their view of the network without removing itself from anyone's reach.
Withholding from a *client* is a different matter and is not mitigated at
all — see §3. One node is one node's opinion.

Note that one recovery response per connection is itself a bounded form of
withholding, chosen deliberately: a peer whose gap is wider than one
response does not converge from the event log and needs a snapshot, which
is the same boundary `EventStore::prune_before` already draws.

### Volume spam by an origin that holds a key

Nothing rate-limits how many well-formed, correctly-signed, distinct
events one identity may originate. Every one costs a signature
verification, a store write and a forward.

This is not closed, and the reason is that closing it does not work.
Rate-limiting per origin is an exclusion mechanism wearing a bound's
clothing: keys are free, so an attacker rotates identities and pays
nothing, while an honest bursty producer — a market-data provider during
a volatile minute — is exactly who the limit fires on. What *is* bounded
is the cost per event: 1 MiB per envelope (`MAX_ENVELOPE_BYTES`), one
signature verification, one `MAX_TTL`-bounded traversal per distinct
event, and a store that a timed sweep holds to a week. Spam therefore
costs bandwidth for as long as it is sustained and leaves nothing behind.
That is the honest trade, not an oversight.

What *is* bounded is the per-**link** variant of this: a single connected
peer flooding us is throttled by the per-peer relay credit (§2). That is a
transport bound, not a per-origin one, so it does not contradict anything
above — key rotation defeats a per-origin limit and is exactly why there
isn't one, but it does not give an attacker more *links*.

### Priority is decorative

`Priority` rides outside the signature and nothing in this workspace
schedules on it, so raising it achieves nothing today. If anything ever
does schedule on it, it becomes a free upgrade for whoever asks last, and
must be re-derived locally from the event type rather than read off the
envelope.

### Connection-count limits are not gossip's to impose

`GossipService` holds one small entry per connected peer, which is far
less than the swarm holds for the connection itself. Bounding how many
connections a node accepts belongs in `openfiat_network` — and is now
implemented there via libp2p's connection limits (§2), not in gossip.

### Two nodes on one wallet

Detected, not prevented, and only from the one vantage point that can see
it — the node whose identity is being used
(`GossipService::is_impostor`). An event signed by our key that we did not
emit is refused and the operator is told. Nothing stops the other machine
from existing; nothing could.

---

## 5. Where the tests are

| Attack | Test |
|---|---|
| Mint many events from one signature by rewriting the id | `one_signature_cannot_be_ground_into_many_events_by_rewriting_the_id` |
| A single id-forged event | `an_event_whose_id_is_not_its_own_content_is_refused` |
| Unprunable far-future stamp | `an_event_stamped_past_any_plausible_clock_is_refused` |
| Honest clock drift is still accepted | `a_slightly_fast_clock_is_still_believed` |
| TTL inflation, and TTL as a censorship lever | `an_inflated_hop_budget_is_cut_back_rather_than_used_to_censor` |
| Replay of a captured event | `a_captured_event_re_pushed_is_not_applied_a_second_time` |
| Recovery-request amplification | `a_flood_of_recovery_requests_is_answered_once_with_the_log_and_then_with_nothing` |
| Recovery above one envelope | `a_log_too_large_for_one_envelope_is_answered_in_part_rather_than_not_at_all` |
| Stream-slot leak via unhandled message type | `a_gossip_message_type_this_node_does_not_implement_is_still_answered` |
| Unbounded invented addresses | `a_peer_inventing_a_new_address_every_time_cannot_grow_this_node_without_bound` |
| Multi-hop relay validates without pre-shared keys | `an_event_relayed_from_an_origin_this_node_never_connected_to_still_validates` |
| An event signed by our own key we did not emit | `service::identity_conflicts::*` |
| One peer deciding this node's public address | `service::corroboration::*` |

Each was checked by removing the mitigation and confirming the test fails —
a test that passes against unfixed code is documentation, not a defence.
