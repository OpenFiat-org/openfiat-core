# A node's region stays declared, not determined

**Question (#173):** a `ServiceRecord`'s `region` is self-declared and
unverified. Could a node's region be *determined* instead — from the
addresses the node already announces, via a GeoIP database or an RIR
allocation table?

**Answer: no, and the field stays declared.** What follows is the
reasoning, so that the next person to ask does not have to redo it, and
so that anyone who disagrees can argue with the actual objections.

## What was investigated

Three ways to derive a region from what a node already publishes.

1. **GeoIP database** (MaxMind GeoLite2 or equivalent), shipped with or
   downloaded by the node, resolving the endpoint's IP to a country.
2. **RIR allocation table** — the five regional internet registries'
   `delegated-extended` files, mapping an address block to the country
   the block was *allocated to*.
3. **A third-party lookup API**, queried at registration or render time.

Option 3 is disqualified immediately and for a reason that has nothing to
do with accuracy: it would make every node, or every viewer of a provider
directory, a client of a company that then learns who is looking up whom.
This project already removed a public IPFS gateway for exactly that
(`lib/ipfs/gateway.ts`), and reintroducing the same shape one layer over
would be a regression, not a feature. It is not discussed further.

Options 1 and 2 are the serious ones. They fail for four separate
reasons, any one of which is sufficient.

## 1. It answers a different question than the one the field asks

OFS-1500 §10 is titled *Geographic Coverage* and says: "Providers MAY
advertise regions served," with a worked example of an SMS gateway
declaring Kenya, Uganda, Tanzania and Rwanda but not Europe. The field is
about **who the service is for**, not where its packets terminate.

GeoIP answers "where is this socket." Those are not the same question,
and for the deployment this network actually has they are routinely
different answers: a VPS in Frankfurt serving M-Pesa users in Kenya is
the ordinary case, not the edge case. Deriving `region` from an IP would
take a record that currently says *Kenya* — true, useful, and the thing a
client wants to select on — and replace it with *DE*, which is precisely
correct about a fact nobody asked for and misleading about the one they
did.

A field that reads "determined" and answers the wrong question is worse
than one that reads "declared" and answers the right one, because the
first invites a client to route on it.

## 2. It would break the property that every node derives the same registry

`openfiat-registry` exists on the premise in OFS-1500 §19/§23 that every
node derives its local registry purely by consuming the same signed
events. Two honest nodes given the same gossip must hold the same record,
which is what makes a registry read from any node equivalent to a read
from any other, and what makes `apply_registration` idempotent.

A derived region is computed locally, from a database that is local. Two
nodes with GeoLite2 snapshots a month apart, or one with the database and
one without, produce different `region` values for the same signed
registration. The record would no longer be a function of the events, and
`getProviders` from two nodes would disagree — silently, in a field a
client selects on. The only way to avoid that is to put the derived value
*inside* the signed registration, at which point it is declared again,
just by a node instead of an operator.

## 3. The input mostly is not an IP address

A `PublicApiNode`'s endpoint is an HTTPS URL, because a browser cannot
call a plain-HTTP node from an HTTPS page. So the derivation would have
to resolve the hostname first — a DNS lookup per record, from every node,
repeated as records refresh — and then geolocate whatever came back. For
any node behind a CDN, a reverse proxy or an anycast address, what comes
back is the edge, not the node. The public devnet node is behind TLS
termination today; the answer for it would describe the terminator.

The libp2p multiaddrs are closer to real addresses, but a node behind NAT
announces `--external-addr` — an address its operator declared, because
by construction only something on the far side can observe it. Geolocating
a declared address does not make the result observed; it launders a
declaration through a lookup table and returns it looking like a
measurement.

## 4. The cost is a standing obligation, not a one-off dependency

GeoLite2 is a licensed download requiring an account, a EULA, and
redistribution terms a permissively-licensed open-source node cannot
simply bundle. It is also perishable: MaxMind publishes twice weekly and
an old database is confidently wrong rather than absent. Shipping it
means every node operator inherits a database-refresh cron and a licence,
and a node that skips it reports stale answers with no signal that they
are stale.

The RIR tables avoid the licence — they are public — and buy an answer
that is coarser still: country of *allocation*. A block allocated to a
holder registered in Nairobi and routed in London reads as KE. That is
not an improvement over asking the operator; it is the operator's answer
with extra steps and a delay of years.

Either way the node gains a periodic data-maintenance duty in exchange
for an answer to the wrong question.

## What is done instead

**The declaration stays, and it reads as a declaration everywhere.**
`Registration::region`, `ServiceRecord::region` and `--region` all say so
in their doc comments; `openfiat-app` renders it as "declared, unverified"
on the provider detail page and as a chip under an explicitly-labelled
*Claims* column in the network view, never with a tick and never beside
an observation. Nothing in this workspace routes on it.

**The honest observation already exists, and it is the better answer
anyway.** The question a user actually has is not "where is this node"
but "is this node fast for *me*", and that is measured, not derived —
`components/network/live-network.tsx` contacts every node it lists from
the visitor's own browser and shows the round-trip time. That reading is
taken from where the user is, by the party who cares, at the moment they
care, and it is correct about the Frankfurt-VPS-serving-Kenya case that
defeats every geolocation approach above: if the node is slow for you, it
says so, whatever country any database thinks it is in.

That is the pattern this codebase already follows for the chain-mode
claim — show what the node said, show what it did when asked, and mark
the case where they disagree. Region has no cheap observation to put
beside it, so it stays alone and stays labelled.

## What would change the answer

- OFS-1500 growing a *separate* field for a measured attribute (a
  measured latency matrix, a signed reachability attestation from a
  third party). That is a new field with new semantics, not a
  reinterpretation of this one.
- Region becoming something a client is required to route on, rather
  than something it may prefer on. It is not, and making it so would
  need the verification story first, not after.

Until then, an unverified declaration presented as an unverified
declaration is the truthful shape, and the alternative on offer is a
guess presented as a fact.
