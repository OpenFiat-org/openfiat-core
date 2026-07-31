# openfiat-reviews

Post-trade reviews: after a settlement is done, each counterparty may leave
one review of the other — a 1-5 star rating and up to 500 characters.

Deliberately a **separate crate from `openfiat-reputation`**, because the
two are different kinds of statement. A reputation profile is evidence,
recomputed by every node from signed settlement/dispute events. A review is
an opinion, and its signature proves only who wrote it. A review therefore
never enters the computed score — `openfiat-reputation` does not depend on
this crate, so it cannot, even by accident. Clients show the two side by
side.

**Spec:** OFS-3000 — OpenFiat Reputation Engine (reviews have no spec
number of their own; see `src/protocol.rs`).

## The rule that makes a review worth anything

Only a party to a real, settled trade may review it, only about the other
party, and only once. That is enforced in `record::subject_of`, read out of
the **settlement record** rather than out of anything the review claims
about itself — a review does not even carry the wallet it is about. The
check runs on the read path, because gossip can deliver a review before the
settlement it refers to and a discarded event is never retried.

## Who can read what

- **Anyone** — `PublicReview`: the subject, the stars, the words, the day.
  Not the author, not the settlement id. A review names two people, and
  this network deliberately does not publish who trades with whom (see
  `openfiat_rpc::methods::redaction`); dropping the author alone would not
  be enough, because both parties may review the same trade and a shared
  settlement id would give the edge back.
- **A party, having proved it holds the wallet** — the full records, both
  the ones it wrote and the ones written about it.

## Depends on

- `openfiat-settlement` — the record every authorization decision is read
  from; never mutated.
- `openfiat-crypto`, `openfiat-network` — signing, and deriving a peer id
  from a public key.
- `openfiat-types`, `openfiat-serialization`, `openfiat-storage`.

## Used by

- `openfiat-rpc` — `getReviews`, `getMyReviews`, `sendReviewPublish`
  (registered from `methods/reputation.rs`, alongside `getReputation`).

## Column family

`reviews`, exported as `REVIEWS_COLUMN_FAMILY`. It is replicated domain
state, so it belongs in `openfiat_rpc::state::SNAPSHOT_COLUMN_FAMILIES` —
which is also the list the node binary opens RocksDB with. A registry whose
column family is missing there writes into nothing on a real node while
passing every in-memory test.
