# Post-trade reviews (#103)

After a settlement is done, each counterparty may leave one review of the
other: a 1-5 star rating and up to 500 characters. Implemented in
`crates/reviews`, exposed from `crates/rpc/src/methods/reputation.rs`.

## The three decisions worth knowing

### A review is not a reputation score, and never becomes one

`openfiat-reputation` computes a profile by re-reading settlements,
reservations and disputes that every node already replicates. Every node
derives the same numbers from the same signed events; nobody asserts
anything about themselves. That is evidence.

A review is an opinion. Its signature proves who wrote it and nothing at
all about whether it is true. If a review moved the score, the score would
stop measuring what happened and start measuring what people said — and it
would be cheap to buy: two wallets can trade with each other repeatedly and
five-star each other every time, at the cost of their own escrow
round-trips.

So they are separate crates. `openfiat-reputation` does not depend on
`openfiat-reviews`, which makes the separation structural rather than a
convention someone can forget. `ReputationProfile` has no review field, and
`getReputation` and `getReviews` are different methods returning different
types. **Clients must show both and must not merge them.**

### Only a counterparty of a settled trade may review it

This is the whole security of the feature. Without it the reputation
surface is a place where strangers write about people they have never
traded with.

It is decided by one function — `openfiat_reviews::record::subject_of` —
which reads the **settlement record**, not the review. A review does not
even carry the wallet it is about: it names a settlement, and who it is
about is derived from that settlement's buyer and seller. A record cannot
therefore name a wallet that was never in the trade.

Two details that are easy to get wrong:

- **`Approved` counts as settled, not only `Completed`.** The step from
  `Approved` to `Completed` is local bookkeeping by a node that observed the
  on-chain escrow release. A `GossipOnly` node never observes it, so gating
  on `Completed` alone would make the same review visible on some nodes and
  invisible on others.
- **The check runs on the read path, not the write path.** Gossip has no
  ordering guarantee, so a node can receive a review before the settlement
  it refers to. A write-time check would discard a genuine review
  permanently, and a discarded event is never retried. An unauthorized
  record that is simply never returned to anyone is inert.

### A review names two people, and this network hides who trades with whom

`crates/rpc/src/methods/redaction.rs` (#167) removed the parties from every
public trade record, because in a P2P fiat market "which merchant does this
wallet always return to" is a physical-safety question. Publishing whole
reviews would have handed that graph straight back, permanently.

So there are two reads:

| Method | Who | Returns |
|---|---|---|
| `getReviews(wallet)` | anyone | `about`, `rating`, `comment`, `created_on` (midnight UTC of the day) |
| `getMyReviews(wallet proof)` | a wallet that proved it holds the key | full records — id, settlement, author, about, rating, comment, exact timestamp |

The author **and the settlement id** are both absent from the public view.
Dropping only the author would not work: both parties may review the same
trade, so a shared settlement id would rejoin them. The timestamp is
truncated to the day for the same reason at a weaker strength — two reviews
published within the same minute are a correlation between their subjects.

What this is not: confidentiality. Reviews are gossiped to every node, so
anyone running one reads the raw records. What is protected is the ease of
the query, which is the same thing, and the same amount, that `redaction`
and `counterparties` protect.

## The methods

- `getReviews({wallet})` — open. `PublicReview[]`, newest first.
- `getMyReviews({wallet, public_key, nonce, signature})` — wallet proof
  under the domain separator `openfiat-my-reviews`. Everything this wallet
  wrote plus everything written about it.
- `sendReviewPublish({data})` — base64 JSON of a `SignedReviewPublish`,
  signed client-side by the author's own key. Returns the review id
  (`{settlement}:{author}`).

Errors from `sendReviewPublish`:

| Code | Means |
|---|---|
| `INVALID_IDENTITY_CLAIM` (2001) | not a party to that settlement, or the trade never settled |
| `RESOURCE_ALREADY_EXISTS` (7) | this wallet's review of this trade is already on file |
| `INVALID_PARAMETER` (3) | comment over 500 characters, or containing control/bidi characters |
| `INVALID_SIGNATURE` (1003) | the signature does not verify, or the key does not derive to the named author |

## What a client has to do

1. **Prompt after settlement.** Poll `getMySettlements`, keep the ones in
   `Approved`/`Completed`, subtract the settlements already present in
   `getMyReviews` where the author is this wallet, and prompt for the rest.
   The node does not push a prompt and does not track "unreviewed".
2. **Sign locally.** Build the `Review`, serialize it as JSON, sign those
   bytes with the wallet key, base64 the `SignedReviewPublish` and send it.
   The node never signs on a user's behalf.
3. **Warn before publishing.** The comment is public, permanent, and cannot
   be edited by anyone but its author (a later review of the same trade
   supersedes it) or deleted by anyone at all. Say so in the UI, and count
   characters against the 500 limit before signing rather than surfacing a
   rejection afterwards.
4. **Render as text, never as markup.** Control and bidi characters are
   rejected at publication, but the comment is still arbitrary user text.
5. **Do not average reviews into the reputation score**, and do not present
   a mean rating as though it carried the same weight as
   `trades_completed`. Two separate figures.
6. **Do not expect an author field** on `getReviews`. If a UI needs "you
   reviewed this trade", that is `getMyReviews`.

## Operational note

`crates/reviews` writes to a `reviews` column family, exported as
`openfiat_reviews::REVIEWS_COLUMN_FAMILY` and listed in
`openfiat_rpc::state::SNAPSHOT_COLUMN_FAMILIES` — which is also the list
`openfiat-node` opens RocksDB with, so nothing else needs changing. A
registry whose column family is missing from that list writes into nothing
on a real node while passing every in-memory test;
`state::tests::a_review_survives_a_store_that_only_accepts_declared_column_families`
is the test that would catch it.
