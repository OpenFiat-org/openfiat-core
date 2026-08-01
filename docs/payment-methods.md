# Payment methods (#102)

What a merchant can say they will be paid with. Two halves: the rails this
build ships with per-country suggestions, and the rails merchants define
for themselves. Implemented in `crates/taxonomy`, exposed from
`crates/rpc/src/methods/payment_methods.rs` and `…/reference.rs`.

This page is the client contract. It is written so an SDK or an app can be
built from it without reading Rust.

## The wire shape

Every payment method — compiled-in or merchant-defined — comes back in one
shape:

```json
{
  "id": "builtin:mpesa-kenya",
  "name": "M-Pesa Kenya (Safaricom)",
  "category": "MobileMoney",
  "aliases": ["mpesa", "m-pesa", "safaricom"],
  "countries": ["KE"]
}
```

| field | meaning |
| --- | --- |
| `id` | The only field to key off, and the only one an advertisement stores. Two namespaces, below. |
| `name` | For reading. Never compared, never sent anywhere. For a merchant-defined method this is text that merchant wrote. |
| `category` | One of `MobileMoney`, `BankTransfer`, `Fintech`, `Cash`. For grouping a long list into something a person can read. |
| `aliases` | Lowercase spellings for type-ahead. Never displayed. Always `[]` for a merchant-defined method. |
| `countries` | Country codes the rail is *suggested* in. `null` means this build makes no per-country claim — see below. Always `null` for a merchant-defined method. |

### `id` has exactly two namespaces

- `builtin:<slug>` — a rail compiled into the node, e.g. `builtin:sepa`.
  The slug is a stable column of the catalog and is **not** derived from
  the display name, so correcting a spelling never orphans an
  advertisement.
- `<merchant peer id>:<16 hex chars>` — a definition that merchant
  published. The hex is a digest of the definition itself.

`builtin` can never collide with a peer id: peer ids are base58btc, whose
alphabet omits `l`.

## The four rules a client must follow

### 1. Send ids, never names

`Advertisement.payment_methods` is an array of ids. Sending a display name
is a client bug, and `getPaymentMethod` returns an error rather than
`null` for one specifically so you find out.

This is not fussiness. The field used to be free text the merchant typed,
which meant whoever controlled a name controlled what every advertisement
that chose it appeared to offer — rename your own "Acme Pay" to "PayPal"
and every ad that picked it now claims to take PayPal, without any of them
being touched. Ids fix that by being immutable in both namespaces: a
merchant-defined id is the digest of its own definition, so editing
anything produces a *different* id that no existing advertisement
references. There is no update event and no delete event, because there is
nothing an edit could land on.

### 2. Suggestions order the picker; they never shorten it

`getPaymentMethods` returns `suggested` and `others`. Show `suggested`
first and **show `others` too**. A merchant in Kenya who settles over SEPA
must be able to pick SEPA. A country this build has nothing listed for
gets an empty `suggested` and the whole catalog in `others` — that is an
ordinary answer, not an error.

`countries: null` is likewise an answer and not a gap. It means "no
per-country claim", and is used for two kinds of row: cash and the two
generic bank transfers, which really do exist everywhere; and the global
fintechs (PayPal, Wise, Skrill, Revolut) whose coverage changes constantly
and where a fixed list that is wrong in three countries would *hide* the
rail in those three. Show `null` rows in every country, after the
suggested ones.

### 3. Never present a merchant-defined method as a catalog rail

A merchant-defined method is **globally readable and merchant-scoped**:

- it replicates by gossip to every node, so a counterparty anywhere can
  resolve what an advertisement means;
- it is selectable **only by the merchant who defined it**. The node
  enforces this — an advertisement naming another merchant's definition is
  refused with `INVALID_ADVERTISEMENT`.

So there is no global namespace of merchant-invented names to squat, and
two merchants who both take Acme Pay publish one definition each. The
visible cost is duplication, which is the honest picture: those are two
different merchants' claims about two different accounts.

Clients must render the distinction. In a picker, a merchant's own
definitions arrive under `merchant`, separately from `suggested`/`others`
— keep them separate. When displaying somebody *else's* advertisement,
mark a non-`builtin:` method as defined by that merchant. The node's name
checks (rule 4) reduce look-alikes; they cannot make arbitrary text safe,
and this marker is what carries the remaining weight.

### 4. Names are checked at publication, so render them as text

`sendPaymentMethodDefine` refuses a name that is:

- empty, over 64 characters, or with no letter or digit in it;
- leading/trailing/repeated spaces, or any whitespace other than `U+0020`
  — refused, never trimmed, so the bytes stored are the bytes checked;
- control characters, or bidirectional overrides and isolates
  (`U+202A`–`U+202E`, `U+2066`–`U+2069`) — the rule `crates/reviews`
  applies to comment text, and here newline is a hazard too because a
  method name is a label, not prose;
- invisible characters — zero-width spaces and joiners, soft hyphen, word
  joiners, BOM, the Unicode tag block;
- a **look-alike of a rail this build already ships**. Names are folded to
  a skeleton — lowercased, accents stripped, Cyrillic/Greek confusables
  and fullwidth forms mapped to Latin, `i`/`l`/`1` and `o`/`0` and their
  friends collapsed to one representative per class, separators dropped —
  and a definition whose skeleton matches any catalog name *or alias* is
  refused. `M-Pesa`, `m pesa`, `М-Реѕа`, `Ｍ－Ｐｅｓａ` and `M-Pes4` all
  reduce to `mpesa` and are all refused. So is `Cash`, because that is an
  alias somebody reaches Cash in Person by typing.

Two limits, stated plainly. The fold table is targeted, not the whole of
UTS-39 — a determined impostor can still land on `M-Pesa Kenya —
Official`, whose skeleton genuinely differs. And nothing is checked
against *other merchants'* definitions, because that check would depend on
which records a node had received, and two nodes would then accept and
refuse different definitions. Rule 3's marker is the backstop for both.

A definition still renders as plain text, always. Never as markup.

## Methods

### `getReferenceData()`

Unchanged in shape; its `payment_methods` array now carries `id` and
`countries`. Cache on `revision`. This is the whole compiled-in catalog
and contains no merchant-defined methods.

### `getPaymentMethods({ country?, wallet? })`

The picker read. Both parameters optional.

```json
{
  "country": "KE",
  "suggested": [ /* compiled-in rails listed for KE */ ],
  "others":    [ /* every other compiled-in rail */ ],
  "merchant":  [ /* definitions the `wallet` merchant published */ ]
}
```

`wallet` is a base64-encoded peer id, like every other `wallet` parameter
on this surface. The read is open, not gated: a definition is a public
replicated record, and a counterparty has to be able to resolve one.

There is deliberately no method that lists other merchants' definitions
for browsing. They are not selectable by you, and an index of arbitrary
merchant text is the shape of the squatting problem scoping avoids.

### `getPaymentMethod({ id })`

Turns one id back into a name — the direction a buyer reading somebody
else's advertisement needs.

- a `builtin:` id this build knows → the catalog row;
- a merchant id this node holds → that definition;
- either kind this node has not got → `null`. **Render the id.** For a
  merchant-defined method `null` may simply mean gossip has not delivered
  it here yet, and the advertisement is valid either way;
- a malformed id → an error, not `null`.

### `sendPaymentMethodDefine({ data })`

`data` is base64 of the JSON-serialized `SignedPaymentMethodDefine` the
caller's own wallet signed:

```json
{
  "method": {
    "merchant": "<peer id>",
    "merchant_public_key": "<base58 key>",
    "name": "Sacco Standing Order",
    "category": "BankTransfer"
  },
  "signature": "<base58 signature>"
}
```

The signature is over the canonical JSON of `method`. The public key must
derive to `merchant`. Returns the new id — which is what to put on an
advertisement.

There is no timestamp and no client-chosen id, both deliberately: the
record is content-addressed, so republishing the identical definition is a
no-op rather than a conflict, and two nodes can never hold different
records under one id.

A merchant may hold 32 definitions. Past that, the 32 with the
lexicographically smallest ids are kept — a rule every node applies to the
same set regardless of the order gossip delivered them in — and a further
definition is refused with `RATE_LIMIT_EXCEEDED`. The bound is per wallet
and cannot be used against anyone else's.

## What the node deliberately does not do

**It does not validate an id against the catalog.** `builtin:some-rail-added-next-year`
is accepted and stored on an advertisement exactly like one this build
knows. A membership check would mean a node one release behind rejecting a
perfectly good advertisement, and two honest nodes disagreeing about which
advertisements exist — the same reasoning `FiatCurrency` gives for
checking the *form* of a currency code and never its membership of a list.

**It does not decide where a merchant may trade.** `countries` orders a
picker. Nothing filters on it.
