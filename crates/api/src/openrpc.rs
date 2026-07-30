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
    "getCheckpointHeight",
    "getChainStatus",
    "getLatestBlockhash",
    "getPeers",
    "getSettledVolume",
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
            json!({
                "name": name,
                "summary": humanize(name),
                "params": [{ "name": "params", "schema": params_schema_for(name) }],
                "result": { "name": "result", "schema": result_schema_for(name) },
            })
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
            "getHeldContent",
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
