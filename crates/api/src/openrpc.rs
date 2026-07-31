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
                "payment_methods": { "type": "array", "items": { "type": "object", "properties": { "name": { "type": "string" }, "category": { "type": "string", "enum": ["MobileMoney", "BankTransfer", "Fintech", "Cash"] }, "aliases": { "type": "array", "items": { "type": "string" }, "description": "lowercase spellings for type-ahead; never shown" } } } },
                "mints": { "type": "array", "items": { "type": "object", "properties": { "mint": { "type": "string", "description": "base58 mint address — the only field that identifies anything; look up by this, never by symbol" }, "symbol": { "type": "string", "description": "a nickname, cluster-dependent and spoofable — `USDC` names a different address on each cluster, and this network's wrapped SOL is named `wSOL`, not `SOL`" }, "decimals": { "type": "integer", "description": "base-unit exponent, carried beside the symbol so a client cannot name a mint while guessing how to scale it" } } }, "description": "mints this build can put a name to. NOT the settlement allowlist — that lives on chain in the escrow program's FeeConfig, is governance-updatable, and the two sets are not guaranteed equal in either direction" },
            },
            "required": ["revision", "currencies", "countries", "payment_methods", "mints"],
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
            "getProvider",
            "getOracleRecord",
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
