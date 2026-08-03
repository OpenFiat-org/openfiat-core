//! Generates an OpenRPC 1.2.6 document — the JSON-RPC equivalent of an
//! OpenAPI/Swagger spec — describing every method `openfiat-rpc` exposes.
//!
//! The method *list* is derived directly from `openfiat_rpc::methods::
//! build_table`, the exact dispatch table the server runs, so a method
//! can never go undocumented or a doc entry drift onto a method that no
//! longer exists — `tests/contract.rs` asserts the two stay in lockstep.
//! Per-method parameter/result *schemas* are a deliberately simplified,
//! convention-based best effort (every `getX(id)` method takes
//! `{id: string}`, every `sendX` method takes `{data: <base64 wire
//! bytes>}`, ...) rather than full JSON Schema derived from each
//! method's concrete Rust types — doing that properly would mean adding
//! a schema-derivation dependency and a `#[derive]` to dozens of already-
//! shipped domain types across this workspace for a documentation nicety.
//! Worth doing later if the type surface asks for it; not required to
//! give third-party integrators a real, honest reference today.

use openfiat_rpc::methods::build_table;
use openfiat_storage::mem::MemoryStore;
use serde_json::{Value, json};

const WALLET_PARAM_METHODS: &[&str] = &[
    "getCounterpartiesChallenge",
    "getWalletChallenge",
    "getIdentityClaimsByWallet",
    "getReputation",
    "getReviews",
    "getSubscription",
    "getDeliveryReceiptsByWallet",
    "getRiskRecordsByWallet",
    "getWalletScreening",
    "getSessionsByWallet",
    "getNotificationDispatchesByWallet",
];

/// Reads that answer only for a wallet the caller proves they control.
///
/// Their params are an ownership proof, not a lookup key, and describing
/// them as `getX(id)` would tell an integrator they are ordinary open
/// reads — misleading for them and, worse, for anyone auditing what this
/// node exposes. `getCounterparties` was special-cased here first; the
/// `getMyX` family arrived with the trade-graph redaction and made it a
/// category rather than an exception.
const WALLET_PROOF_METHODS: &[&str] = &[
    "getCounterparties",
    "getMySettlements",
    "getMyReservations",
    "getMyDisputes",
    "getMyTrades",
    "getMyReviews",
];

const NO_PARAM_METHODS: &[&str] = &[
    "getVersion",
    "getHealth",
    "getAdvertisements",
    "getReservations",
    "getSettlements",
    "getTrades",
    "getDisputes",
    "getProposals",
    "getProviders",
    "getOracleRecords",
    "getSnapshots",
    "getLatestSnapshot",
    "getCheckpointSlot",
    "getChainStatus",
    "getLatestBlockhash",
    "getPeers",
    "getSettledVolume",
    "getReferenceData",
];

/// One payment method, as both the reference read and the picker read
/// return it.
///
/// Written once because the two must not drift: a client that parsed
/// `getReferenceData`'s rows and `getPaymentMethods`' rows with different
/// expectations would work until the day a field moved.
fn payment_method_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": { "type": "string", "description": "what an advertisement stores, and the only field to key off. Either `builtin:<slug>` for a rail compiled into the node, or `<merchant peer id>:<digest>` for one that merchant defined. Never send a name where an id is asked for" },
            "name": { "type": "string", "description": "for reading. Never compared, never stored on an advertisement, and for a merchant-defined method it is text that merchant wrote — render it as theirs" },
            "category": { "type": "string", "enum": ["MobileMoney", "BankTransfer", "Fintech", "Cash"] },
            "aliases": { "type": "array", "items": { "type": "string" }, "description": "lowercase spellings for type-ahead; never shown. Always empty for a merchant-defined method" },
            "countries": { "type": ["array", "null"], "items": { "type": "string" }, "description": "country codes this rail is suggested in. `null` means this build makes no per-country claim — offer it everywhere, after the suggested ones — and is used for cash, generic bank transfer, and the global fintechs whose coverage a fixed list would get wrong" },
        },
        "required": ["id", "name", "category", "aliases"],
    })
}

fn params_schema_for(method: &str) -> Value {
    // The one read on this surface that answers only for the caller's own
    // wallet, so its params are the ownership proof rather than a lookup
    // key. Documented explicitly because the `getX(id)` fallback below
    // would otherwise describe it as an ordinary open read — misleading
    // for an integrator and, worse, for anyone auditing what this node
    // exposes.
    if WALLET_PROOF_METHODS.contains(&method) {
        return json!({
            "type": "object",
            "properties": {
                "wallet": { "type": "string", "description": "base64-encoded PeerId — must be the wallet the signature below proves control of; any other wallet is refused rather than narrowed" },
                "public_key": { "type": "string", "description": "base64-encoded raw 32-byte Ed25519 public key, which must derive to `wallet`" },
                "nonce": { "type": "string", "description": "the nonce from a preceding getWalletChallenge — single-use and expiring" },
                "signature": { "type": "string", "description": "base64-encoded 64-byte Ed25519 signature over the challenge under this method's own domain separator; a signature made for another gated method does not verify here" },
            },
            "required": ["wallet", "public_key", "nonce", "signature"],
        });
    }
    // A wallet proof *plus* a lookup key — the only method on this
    // surface that is both. It reads one trade's confidential channel, so
    // it needs to know which trade and who is asking, and describing it
    // as either alone would be wrong in a way an integrator only
    // discovers at runtime.
    if method == "getMyTradeChannel" {
        return json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "the settlement id whose channel is being read" },
                "wallet": { "type": "string", "description": "base64-encoded PeerId — must be a party to that settlement, or hold a key grant on the channel (which is how an arbitrator reads a disclosed one)" },
                "public_key": { "type": "string", "description": "base64-encoded raw 32-byte Ed25519 public key, which must derive to `wallet`" },
                "nonce": { "type": "string", "description": "the nonce from a preceding getWalletChallenge — single-use and expiring" },
                "signature": { "type": "string", "description": "base64-encoded 64-byte Ed25519 signature over the challenge under this method's own domain separator" },
            },
            "required": ["id", "wallet", "public_key", "nonce", "signature"],
        });
    }
    // A service provider reading their own earnings statement: an id plus
    // a signed nonce, not a bare lookup. It predates the `getMyX` family
    // and uses `id`/`nonce`/`signature` rather than the four-field wallet
    // proof, so it is described on its own rather than folded in — the
    // document should say what the method takes, not what it resembles.
    if method == "getProviderEarnings" {
        return json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "the service id whose statement is being read" },
                "nonce": { "type": "string", "description": "the nonce from a preceding getProviderEarningsChallenge — single-use and expiring" },
                "signature": { "type": "string", "description": "base64-encoded 64-byte Ed25519 signature by the provider key that registered the service" },
            },
            "required": ["id", "nonce", "signature"],
        });
    }
    // A service id plus the token the caller wants the fee priced in.
    // Described here rather than left to the `getX(id)` fallback, which
    // would tell an integrator the second parameter does not exist and
    // leave them wondering why every quote came back in the wrong token.
    if method == "getProviderFeeQuote" {
        return json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "the service id whose declared fee is being priced" },
                "settlement_mint": { "type": "string", "description": "base58 SPL mint of the token to settle in — the mint, never a symbol. A mint this build cannot scale comes back as status `unsettleable`/`UnknownSettlementToken` rather than being approximated" },
            },
            "required": ["id", "settlement_mint"],
        });
    }
    // The two content reads. They take a `cid`, not an `id`, and the
    // fallback below published it as `id` for as long as `getHeldContent`
    // has existed — a documented parameter name that no caller could have
    // used.
    if method == "getHeldContent" || method == "getContentFile" {
        return json!({
            "type": "object",
            "properties": { "cid": { "type": "string", "description": "a CIDv1 base32 sha2-256 content address" } },
            "required": ["cid"],
        });
    }
    // A picker read: both parameters are optional and neither is a
    // lookup key, so the `getX(id)` fallback would describe it as a read
    // of one record by id — which is not a mistake a caller recovers from
    // by experiment, because omitting both is a legitimate call that
    // returns the whole catalog.
    if method == "getPaymentMethods" {
        return json!({
            "type": "object",
            "properties": {
                "country": { "type": ["string", "null"], "description": "a country code from getReferenceData. Decides only what is *suggested*: nothing is withheld from a country, and an unrecognised code returns the whole catalog with nothing suggested" },
                "wallet": { "type": ["string", "null"], "description": "base64-encoded PeerId whose own definitions to include under `merchant`. Open, not gated — a definition is a public replicated record, and a counterparty has to be able to resolve what an advertisement means" },
            },
        });
    }
    if method == "getRewardObservations" {
        return json!({
            "type": "object",
            "properties": { "epoch": { "type": ["integer", "null"], "description": "omit for the most recently completed epoch — the in-flight one's answer would change under the caller" } },
        });
    }
    if method == "getMedianExchangeRate" || method == "getExchangeRate" {
        return json!({ "type": "object", "properties": { "base": { "type": "string" }, "quote": { "type": "string" } }, "required": ["base", "quote"] });
    }
    if NO_PARAM_METHODS.contains(&method) {
        return json!({ "type": "object" });
    }
    if WALLET_PARAM_METHODS.contains(&method) {
        return json!({ "type": "object", "properties": { "wallet": { "type": "string", "description": "base64-encoded PeerId" } }, "required": ["wallet"] });
    }
    if method.starts_with("send") {
        return json!({
            "type": "object",
            "properties": { "data": { "type": "string", "description": "base64-encoded, already-signed wire payload — the caller's own wallet signs it; this node only decodes and applies it" } },
            "required": ["data"],
        });
    }
    // Every remaining `getX(id)` method. Reached by falling through, so
    // it is also what an unclassified new method silently becomes — see
    // `no_method_is_documented_by_accident` for why that matters and what
    // stops it.
    json!({ "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"], "x-openfiat-classified": "fallback" })
}

fn result_schema_for(method: &str) -> Value {
    // The `getX` fallback below would describe this one as "null if not
    // found", which is wrong twice over: it is a compiled-in table, so it
    // is never absent, and an integrator told the answer may be null
    // writes a branch that can only ever be dead code.
    if method == "getReferenceData" {
        return json!({
            "type": "object",
            "properties": {
                "revision": { "type": "string", "description": "digest of the four lists below — changes only when they do, so a client can cache on it and two nodes can be compared for agreement by one string" },
                "currencies": { "type": "array", "items": { "type": "object", "properties": { "code": { "type": "string" }, "name": { "type": "string" }, "symbol": { "type": "string" } } } },
                "countries": { "type": "array", "items": { "type": "object", "properties": { "code": { "type": "string", "description": "ISO 3166-1 alpha-2, or a stable pseudo-code (XNC, XTR) for a territory that has none — do not assume two characters" }, "name": { "type": "string" }, "currency": { "type": "string" }, "alt_currencies": { "type": "array", "items": { "type": "string" }, "description": "other currencies in genuine circulation, most-used first" } } } },
                "payment_methods": { "type": "array", "items": payment_method_schema() },
                "mints": { "type": "array", "items": { "type": "object", "properties": { "mint": { "type": "string", "description": "base58 mint address — the only field that identifies anything; look up by this, never by symbol" }, "symbol": { "type": "string", "description": "a nickname, cluster-dependent and spoofable — `USDC` names a different address on each cluster, and this network's wrapped SOL is named `wSOL`, not `SOL`" }, "decimals": { "type": "integer", "description": "base-unit exponent, carried beside the symbol so a client cannot name a mint while guessing how to scale it" } } }, "description": "mints this build can put a name to. NOT the settlement allowlist — that lives on chain in the escrow program's FeeConfig, is governance-updatable, and the two sets are not guaranteed equal in either direction" },
            },
            "required": ["revision", "currencies", "countries", "payment_methods", "mints"],
        });
    }
    if method == "getPaymentMethods" {
        return json!({
            "type": "object",
            "properties": {
                "country": { "type": ["string", "null"], "description": "echoed back, so a cached answer says which country it was for" },
                "suggested": { "type": "array", "items": payment_method_schema(), "description": "compiled-in rails this build suggests in `country`. Empty is an ordinary answer" },
                "others": { "type": "array", "items": payment_method_schema(), "description": "every other compiled-in rail, still selectable — a suggestion is an ordering, never a restriction" },
                "merchant": { "type": "array", "items": payment_method_schema(), "description": "definitions the `wallet` merchant published. Selectable only by that merchant; show them as theirs, never mixed in with the catalog" },
            },
            "required": ["suggested", "others", "merchant"],
        });
    }
    if method == "getPaymentMethod" {
        return json!({
            "type": ["object", "null"],
            "description": "the method one id names, or null when this node has never received it — an ordinary answer for a merchant-defined rail that has not replicated here yet, not an error. A malformed id *is* an error, so a client that sent a display name finds out.",
            "properties": payment_method_schema()["properties"].clone(),
        });
    }
    if method == "getHeldContent" {
        return json!({
            "type": "object",
            "properties": { "content": { "type": ["string", "null"], "description": "base64 of the block this CID names, or null when this node does not hold it — an ordinary answer, not an error" } },
        });
    }
    if method == "getContentFile" {
        return json!({
            "type": "object",
            "properties": { "content": { "type": ["string", "null"], "description": "base64 of the whole file, chunks concatenated in DAG order, or null when any block of it is missing — half a file is never returned" } },
        });
    }
    if method.starts_with("send") {
        json!({ "description": "the created/updated resource's ID, or nothing meaningful to return for a state-transition-only method" })
    } else if let Some(name) = method.strip_prefix("get")
        && name.ends_with('s')
    {
        json!({ "type": "array", "items": { "type": "object" }, "description": "see the corresponding record type in the owning openfiat-core crate" })
    } else {
        json!({ "type": ["object", "null"], "description": "see the corresponding record type in the owning openfiat-core crate; null if not found" })
    }
}

/// The few methods whose contract is not implied by their name.
///
/// Deliberately short. A summary derived from the method name is honest
/// for a lookup and useless for a method whose *point* is how the caller
/// is meant to use the answer.
fn description_for(method: &str) -> Option<&'static str> {
    match method {
        "getProposalChainLink" => Some(
            "Which on-chain proposal this off-chain proposal is the other half of, and whether \
             the two actually name each other. \
             Governance runs in two places. Proposals are gossiped, signed off-chain records \
             keyed by an author-chosen string; the `openfiat-governance` Solana program holds \
             `Proposal` accounts keyed by a u64, and it is the program's stake-weighted tally \
             that decides an outcome. Nothing correlated the two, so an interface could show \
             one and imply the other with no way to notice a disagreement. This is the join. \
             It takes two claims, and both must hold. The off-chain proposal names the \
             on-chain id inside the `ProposalCreate` event its author signed at creation \
             (`onchain_proposal_id` here). The on-chain proposal names the off-chain one back, \
             by storing `offchain_id_hash` — the SHA-256 of the off-chain id — via the \
             program's `link_offchain_proposal`. Either claim alone proves nothing: anyone may \
             create an on-chain proposal naming an id they do not own. `agreement` reports \
             which case you are in, and `ClaimNotReciprocated` means the records are not \
             joined and the chain's tally says nothing about this proposal. \
             `agreement` reflects what this node has already adopted, not a live account read \
             — these handlers hold no chain client — so it reads `ClaimNotReciprocated` on a \
             node that has not fetched the account yet. `onchain_proposal_address` is returned \
             precisely so a client can fetch the account itself rather than re-derive a PDA \
             that has to agree with the program byte for byte. \
             `offchain_id_hash` is also exactly what to pass to `link_offchain_proposal` when \
             creating the chain-side half.",
        ),
        "sendReservationCancel" => Some(
            "The taker's way out of a reservation, before the validation window runs out. \
             Send a base64 `SignedReservationCancel`: `{cancel: {id, requester, timestamp}, \
             signature}`, where the signature is over the canonical JSON of `cancel` alone \
             (not the envelope) under the requester's own key. \
             Only the requester may cancel, and both halves are checked against the stored \
             reservation rather than against anything in the payload: `requester` must equal \
             the reservation's own requester, and the signature must verify under the public \
             key that reservation already carries. A key supplied in the payload is never \
             consulted, so naming someone else's reservation gets you nothing. \
             Legal only from `EscrowLocked`, the only non-terminal reservation state — \
             cancelling one already `Cancelled` or `Expired` returns an application error \
             rather than succeeding quietly. On success the merchant's advertised liquidity \
             is credited back immediately and the reservation moves to `Cancelled`; the \
             alternative, and the only option before this method existed, was waiting out the \
             full thirty-minute window so the node's expiry sweep did it for you. \
             There is no merchant-side cancel. A merchant who wants their liquidity back \
             waits for the window; that is the cost of publishing an advertisement, and \
             letting them cancel would make every reservation revocable by the counterparty \
             it is supposed to bind. \
             Note for anyone building a settlement flow on top: this cancels the reservation \
             only. It does not consult, or move, any settlement raised against that \
             reservation — see `sendSettlementCancelled` for that, and send both if you mean \
             to abandon a trade that has already reached settlement.",
        ),
        "sendSettlementRejected" => Some(
            "The merchant's \"I cannot find this payment\" — the counterpart to \
             `sendSettlementApproved`, and the alternative to opening a dispute over it. \
             Send a base64 `SignedSettlementRejected`: `{action: {settlement_id, seller, \
             reason, discrepancy, timestamp}, signature}`, signed over the canonical JSON of \
             `action` under the seller's key. `reason` is free text for a human reading the \
             trade; `discrepancy` is the machine-readable one and is what reputation counts \
             — one of `IncorrectAmount`, `WrongReference`, `DuplicatePayment`, \
             `IncorrectAccount`, `Other`. Both are required, and picking `Other` when a \
             specific kind applies costs the counterparty a legible record rather than \
             costing you anything. \
             Legal only from `PaymentSubmitted`, and only under the seller on file. There is \
             nothing to reject before the buyer has declared payment, and the buyer cannot \
             reject their own settlement. \
             Rejection is not arbitration and does not pretend to be: it is the merchant's \
             claim, recorded and gossiped, and it moves the settlement to `Rejected`. A buyer \
             who really did pay is not out of options — `sendDisputeOpen` accepts a \
             settlement in any state, so the dispute path stays open afterwards. What changes \
             is who pays to escalate. Before this method existed a merchant's only way to \
             refuse was to open the dispute themselves, which meant a filing fee, arbitrators \
             and a frozen escrow to say no to a payment that never arrived.",
        ),
        "sendSettlementCancelled" => Some(
            "Either party walks away from a settlement, before any payment is declared. \
             Send a base64 `SignedSettlementCancelled`: `{action: {settlement_id, canceller, \
             timestamp}, signature}`, signed over the canonical JSON of `action` under the \
             canceller's key. \
             `canceller` must be the settlement's own buyer or seller; the node picks which \
             public key to verify against by matching that field against the stored record, \
             so a third party naming themselves canceller is refused before any signature is \
             examined, and a party signing under the other's name fails the check that \
             follows. \
             Legal only from `AwaitingPayment`. Once the buyer has sent \
             `sendPaymentSubmitted` this method returns an invalid-state error, and the \
             merchant's remaining moves are approval, `sendSettlementRejected`, or a dispute \
             — none of which can be taken unilaterally and silently. That restriction is the \
             whole security property here, so do not design a client around cancelling late. \
             The one gap it cannot close is the interval between a buyer actually wiring \
             fiat and that buyer declaring it: a merchant may cancel inside it. Submit \
             `sendPaymentSubmitted` before the money leaves rather than after it lands — the \
             declaration costs a buyer nothing, and the window it closes costs them the whole \
             transfer.",
        ),
        "getMyTradeChannel" => Some(
            "One trade's confidential channel: the payment details one party handed the other, \
             and the conversation they had. Everything it returns is ciphertext. This node \
             holds no key that opens any of it and neither does any other node — decryption is \
             entirely the client's, and there is no server-side read path that could be \
             persuaded to do it. \
             The channel key is a random 32 bytes per trade, generated by a client and never \
             sent anywhere in the clear. `grants` carries it sealed to each permitted reader \
             with the same `openfiat_crypto` sealed box `sendSubscriptionUpdate` uses for \
             notification destinations: open the grant addressed to your own wallet with your \
             own private key, and the key inside opens every entry. `entries` are those \
             encrypted payloads, each bound to its settlement, author, sequence number and \
             kind — a payload moved to any other slot fails to decrypt rather than opening \
             into the wrong place. Payloads are padded, so a ciphertext's length says little \
             about what it holds. \
             Who may read a channel is public even though the contents are not: `grants` names \
             every reader, so a party who has not disclosed to an arbitrator cannot pretend \
             otherwise. Publish with `sendTradeChannelKeyGrant` and `sendTradeChannelEntry`. A \
             grant may only be addressed to the trade's own buyer or seller, or to an \
             arbitrator who has already joined a dispute over it — and it cannot be revoked, \
             because the recipient already holds bytes every node on the network has a copy \
             of. \
             Disclosure to an arbitrator is all or nothing by design: one grant opens the \
             entire channel, including the payment details and every message written before \
             the dispute existed. There is no way to hand over part of a conversation, which \
             is the point — a curated transcript is an argument, not evidence.",
        ),
        "getReviews" => Some(
            "What people who traded with this wallet said about it: a 1-5 star rating, up to \
             500 characters, and the day it was written. Deliberately not the author and not \
             the settlement — a review names two people, and publishing both ends would \
             rebuild the trade graph that `getSettlements` and `getCounterparties` refuse to \
             hand out. Both parties may review the same trade, so a settlement id here would \
             give the pairing away even with the author removed; that is why it is absent \
             too, and why the timestamp is truncated to the day. \
             This is opinion, not evidence: it is signed, so you know somebody who really \
             traded with this wallet wrote it, and nothing more than that. It is deliberately \
             kept out of `getReputation`, which is recomputed by every node from signed \
             settlement and dispute events and cannot be talked up or down. Show both; do not \
             merge them. Publish with `sendReviewPublish`, which only a party to a settled \
             trade may do, once per trade, about the other party.",
        ),
        "getPaymentMethods" => Some(
            "What to put in a merchant's payment-method picker, for the country they are in. \
             `suggested` is what this build lists for that country — M-Pesa in Kenya, Pix in \
             Brazil, SEPA in Germany — and `others` is every other rail it ships. Show both: a \
             suggestion is an ordering and never a restriction, and a merchant who settles over \
             a corridor nobody anticipated must still be able to say so. A country this build \
             has nothing listed for gets an empty `suggested` and the whole catalog, which is an \
             answer rather than an error. \
             `merchant` is the rails that wallet defined for itself: signed records that \
             replicate to every node, so the counterparty who has to pay one can resolve it. \
             They are selectable ONLY by the merchant who defined them — an advertisement naming \
             another merchant's definition is refused — and a client must render them as that \
             merchant's own rather than mixed in with the catalog. There is deliberately no \
             method for browsing other merchants' definitions: they are not yours to choose, and \
             an index of arbitrary merchant text is exactly the name-squatting surface scoping \
             them avoids. \
             Key off `id` and never off `name`. An advertisement stores ids; a name is for \
             reading and, for a merchant-defined method, is text somebody typed. Publish with \
             `sendPaymentMethodDefine`, which refuses control characters, bidirectional \
             overrides, invisible characters, and any name that folds to the same skeleton as a \
             rail this build already ships. Resolve one id with `getPaymentMethod`. The full \
             contract is in docs/payment-methods.md.",
        ),
        "getReferenceData" => Some(
            "The countries, fiat currencies, payment methods and token mints to offer a user to choose from: \
             one list every client reads, instead of each interface shipping its own copy and \
             two honest builds disagreeing about what the network supports. Adding a payment \
             method is a node update, not a release of every app. \
             It is a suggestion list and never a validation gate. Nothing on this surface \
             consults it: an advertisement in a currency absent from `currencies` is accepted \
             exactly as one that is present, because a currency code is checked for form and \
             deliberately not for membership of any list — a node built last year must not \
             reject an advertisement in a currency added since. Do not use this to decide what \
             is permitted, and do let a user name a rail it does not list. \
             `mints` is the same kind of answer for token addresses: what this build calls each \
             mint, so an interface shows `USDC` rather than `2bHPi5hA4z…`. Look mints up by \
             address; a symbol is a nickname, is cluster-dependent, and travels on no record \
             this protocol carries. In particular this network settles wrapped SOL under the \
             name `wSOL`, so a client routing on `SOL` matches nothing. This is emphatically \
             NOT the settlement allowlist: that lives on chain in the escrow program's \
             `FeeConfig`, governance can change it, and the two sets are not guaranteed equal \
             in either direction. \
             The lists are compiled into the node rather than derived from anything, so they \
             are still hand-maintained tables; they are one set of tables. Cache on `revision`, \
             which changes when and only when the data does.",
        ),
        "getContentFile" => Some(
            "Fetch a whole file this node holds, addressed by CID: the DAG is walked here and \
             the chunks come back concatenated, one call instead of forty. The caller cannot \
             check the result — above 256 KiB a CID names a dag-pb root whose digest covers \
             the root node and not the file, so there is no check to perform even in \
             principle. Use `getHeldContent` for anything that has to be verifiable, such as \
             evidence in a dispute. This method exists for viewers that verify nothing \
             regardless: `GET /ipfs/{cid}` on this same host is its HTTP shape, and is what \
             an <img> tag should point at. Returns null if any block is missing — never a \
             partial file.",
        ),
        "getHeldContent" => Some(
            "Fetch one IPFS block this node holds, addressed by CID. This is the trustless \
             content read: the answer is bytes, and the caller checks them by hashing — \
             sha2-256 of the block must equal the digest inside the CID it asked for. A node \
             that returns anything else is caught by that check, so no trust in this node is \
             required or implied. \
             Content at or under 256 KiB is a single block and this is the whole file. Above \
             that the CID names a dag-pb root: fetch it here, read its links (PBNode field 2, \
             each PBLink's field 1 being a binary CID), fetch each linked block by that CID, \
             and check every one the same way. Do not trust a link list from anywhere but a \
             root block you have already hashed — that is what keeps the walk trustless.",
        ),
        "getProviderFeeQuote" => Some(
            "What a service's declared fee costs in some other token right now — a USDC price \
             quoted in OPEN. Answers with a tagged `status`, and every branch matters: `free` \
             (the service declared no price at all), `native` (it already bills in the token you \
             asked for, so no rate is involved), `settleable` (the converted amount, plus the \
             `rate` it came from and the `expiresAt` it is good until), and `unsettleable` with a \
             `reason`. \
             `unsettleable` is a real answer, not an error, and must never be rendered as zero or \
             as the last number you saw. `StaleOracleData` means providers do publish this pair \
             and every record has expired — the feed will likely come back, so waiting is \
             sensible. `NoOracleData` means nobody prices this pair and waiting is pointless. \
             `UnknownSettlementToken` means this build cannot say how many base units one of that \
             token is and refuses to guess. In every case the fee itself is untouched: it is not \
             settleable in that token right now, and remains payable in the token the provider \
             declared. There is no fallback rate anywhere on this path. \
             This is a display read and two nodes may legitimately answer differently, because \
             each resolves against the oracle records it happens to hold. Do not treat it as a \
             commitment: the number a payer is bound to is the one they sign, and `expiresAt` is \
             how long they have to sign it. Past that instant the quote is gone and the payer \
             bears whatever the rate did — ask again. \
             `settlementAmount` is rounded UP, deliberately, so a fee settled in a substitute \
             token is never worth less than the fee that was declared; the payer pays under one \
             base unit more.",
        ),
        "getProviders" => Some(
            "Every service in this node's replica of the Service Registry (OFS-1500). \
             Read the whole answer as claims. A registration is signed by the provider's own \
             key, which proves the record reached you unaltered and proves nothing else: \
             `region` is what the operator typed and nothing observes where the service \
             actually is, `capabilities` is what it says it can do, and `branding` — `name`, \
             `description`, `logo`, `website` — is what it wants to be called. Anyone may \
             register a service named after anyone. Render all of it as declared, and never \
             show a name without the Service ID or provider peer id beside it. \
             `branding.logo` is an IPFS CID and deliberately not a URL: fetch it from \
             `GET /ipfs/{cid}` on the node you are already talking to. Treating it as a URL, \
             or hotlinking a logo from the provider's own host, would report every viewer of \
             your directory to whoever serves that image. \
             `health` and `last_health_update` are the only fields that decay: a provider \
             that stops publishing health updates expires from the registry on its own. There \
             is no uptime percentage and no latency figure anywhere in OFS-1500 — measure \
             latency yourself if you need it, from where your user is.",
        ),
        _ => None,
    }
}

/// Splits `getAdvertisement` into `"get advertisement"` — a genuinely
/// informative default summary with no per-method authoring required.
fn humanize(method: &str) -> String {
    let mut words = String::new();
    for (i, c) in method.char_indices() {
        if i > 0 && c.is_uppercase() {
            words.push(' ');
        }
        words.extend(c.to_lowercase());
    }
    words
}

pub fn build_document() -> Value {
    let table = build_table::<MemoryStore>();
    let methods: Vec<Value> = table
        .method_names()
        .into_iter()
        .map(|name| {
            let mut method = json!({
                "name": name,
                "summary": humanize(name),
                "params": [{ "name": "params", "schema": params_schema_for(name) }],
                "result": { "name": "result", "schema": result_schema_for(name) },
            });
            if let Some(description) = description_for(name) {
                method["description"] = json!(description);
            }
            method
        })
        .collect();

    json!({
        "openrpc": "1.2.6",
        "info": {
            "title": "OpenFiat Node API",
            "version": openfiat_rpc::version(),
            "description": "Solana-style JSON-RPC 2.0 surface over every OpenFiat domain: advertisements, reservations, settlement, trade, disputes, identity, reputation, governance, service providers, notifications, oracles, risk, snapshots, sessions, and the Solana chain bridge. POST to /rpc with {\"jsonrpc\":\"2.0\",\"id\":...,\"method\":...,\"params\":{...}}.",
        },
        "servers": [{ "name": "this node", "url": "/rpc" }],
        "methods": methods,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lists above are hand-maintained, and a method absent from all
    /// of them does not fail — it quietly becomes `getX(id)` in the
    /// published reference. Six methods had already done exactly that
    /// (`getPeers`, `getWalletChallenge`, `getExchangeRate` and three
    /// `getMyX`), so integrators reading the API document were told the
    /// wrong parameters for a wallet-proof read.
    ///
    /// This is the check that turns forgetting into a failing build. The
    /// allowance below is the genuine `getX(id)` family, named
    /// individually: a method may take an id, but somebody has to say so.
    #[test]
    fn no_method_is_documented_by_accident() {
        let document = build_document();
        let fell_through: Vec<String> = document["methods"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["params"][0]["schema"]["x-openfiat-classified"] == "fallback")
            .map(|m| m["name"].as_str().unwrap().to_string())
            .collect();

        // Every one of these genuinely takes a single `id`. Adding a
        // method here is a deliberate statement about its parameters.
        const TAKES_AN_ID: &[&str] = &[
            "getAdvertisement",
            "getReservation",
            "getSettlement",
            "getTrade",
            "getDispute",
            "getProposal",
            "getProposalChainLink",
            "getProvider",
            "getOracleRecord",
            "getPaymentMethod",
            "getRiskRecord",
            "getSnapshot",
            "getSession",
            "getIdentityClaim",
            "getAttachment",
            "getAttachmentsBySettlement",
            "getSubscriptionById",
            "getDeliveryReceipt",
            "getProposalVotes",
            "getNotificationDispatch",
            "getProviderEarningsChallenge",
            "getSettlementAttachments",
        ];

        let unexplained: Vec<&String> = fell_through
            .iter()
            .filter(|name| !TAKES_AN_ID.contains(&name.as_str()))
            .collect();
        assert!(
            unexplained.is_empty(),
            "these methods are documented as taking `id` because nothing \
             said otherwise, which is how six of them ended up described \
             wrongly: {unexplained:?}"
        );
    }

    #[test]
    fn every_dispatch_table_method_appears_in_the_document() {
        let table = build_table::<MemoryStore>();
        let document = build_document();
        let documented: Vec<&str> = document["methods"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["name"].as_str().unwrap())
            .collect();
        for name in table.method_names() {
            assert!(
                documented.contains(&name),
                "{name} is dispatchable but not documented"
            );
        }
        assert_eq!(
            documented.len(),
            table.method_names().len(),
            "documented method count must match the dispatch table exactly — no orphaned entries"
        );
    }

    /// The counterparty read is the only method here that refuses to
    /// answer for a wallet other than the caller's own. If it ever fell
    /// back to the generic `getX(id)` schema, the published reference
    /// would describe a private aggregate as an open lookup.
    #[test]
    fn the_counterparty_read_documents_its_ownership_proof_not_an_id_lookup() {
        let document = build_document();
        let method = document["methods"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["name"] == "getCounterparties")
            .expect("the method must be dispatchable and documented");
        let properties = &method["params"][0]["schema"]["properties"];
        assert!(properties["signature"].is_object());
        assert!(properties["public_key"].is_object());
        assert!(
            properties["id"].is_null(),
            "it must not be described as a generic getX(id) read"
        );
    }

    /// An integrator reading this document decides what to render. If it
    /// described the public review read as an ordinary `getX(id)` lookup,
    /// or said nothing about what a review is worth, the predictable
    /// result is a client that averages reviews into the reputation score
    /// and an SDK that expects an author field this surface will never
    /// send.
    #[test]
    fn the_public_review_read_says_what_it_withholds_and_why() {
        let document = build_document();
        let method = document["methods"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["name"] == "getReviews")
            .expect("the method must be dispatchable and documented");

        let properties = &method["params"][0]["schema"]["properties"];
        assert!(properties["wallet"].is_object(), "{properties}");
        assert!(properties["id"].is_null(), "{properties}");

        let description = method["description"]
            .as_str()
            .expect("a redacted read must explain its redaction");
        assert!(description.contains("not the author"));
        assert!(description.contains("getReputation"));
    }

    /// The picker read takes a country and a wallet, both optional, and
    /// the `getX(id)` fallback would publish it as a read of one record
    /// by id. An integrator following that would send `{"id": "KE"}` and
    /// get the whole catalog back with nothing suggested — a wrong answer
    /// that looks like a right one.
    #[test]
    fn the_picker_read_documents_a_country_and_not_an_id() {
        let document = build_document();
        let method = document["methods"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["name"] == "getPaymentMethods")
            .expect("the method must be dispatchable and documented");

        let properties = &method["params"][0]["schema"]["properties"];
        assert!(properties["country"].is_object(), "{properties}");
        assert!(properties["wallet"].is_object(), "{properties}");
        assert!(properties["id"].is_null(), "{properties}");

        let description = method["description"]
            .as_str()
            .expect("a surface that carries merchant-written text must say so");
        assert!(
            description.contains("selectable ONLY by the merchant"),
            "the scoping rule is the one thing a client cannot infer"
        );
        assert!(description.contains("never off `name`"));

        // And the row shape is the same one the reference read documents,
        // so a client cannot parse the two differently.
        let items = &method["result"]["schema"]["properties"]["suggested"]["items"];
        assert_eq!(items, &payment_method_schema());
        let reference = document["methods"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["name"] == "getReferenceData")
            .unwrap();
        assert_eq!(
            &reference["result"]["schema"]["properties"]["payment_methods"]["items"],
            items
        );
    }

    #[test]
    fn the_content_fallback_names_the_parameter_it_actually_takes() {
        // It was documented as `id` for as long as the method existed,
        // because the `getX(id)` fallback caught it. An interface written
        // against this reference would have got `InvalidParams` and no
        // clue why — a durability fallback that exists and cannot be
        // reached is not one.
        let document = build_document();
        let method = document["methods"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["name"] == "getHeldContent")
            .expect("the method is on the surface");

        let properties = &method["params"][0]["schema"]["properties"];
        assert!(properties["cid"].is_object(), "{properties}");
        assert!(properties["id"].is_null(), "{properties}");
        assert!(
            method["result"]["schema"]["properties"]["content"].is_object(),
            "a caller has to be told the answer is base64 under `content`"
        );
        assert!(
            method["description"]
                .as_str()
                .is_some_and(|d| d.contains("hashing")),
            "the fallback is only trustless if the caller is told to check"
        );
    }

    #[test]
    fn send_methods_document_the_base64_payload_shape() {
        let document = build_document();
        let send_method = document["methods"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["name"] == "sendAdvertisementCreate")
            .unwrap();
        assert!(send_method["params"][0]["schema"]["properties"]["data"].is_object());
    }
}
