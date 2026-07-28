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

const WALLET_PARAM_METHODS: &[&str] =
    &["getIdentityClaimsByWallet", "getReputation", "getSubscription", "getDeliveryReceiptsByWallet", "getRiskRecordsByWallet", "getWalletScreening", "getSessionsByWallet"];

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
];

fn params_schema_for(method: &str) -> Value {
    if method == "getMedianExchangeRate" {
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
    // Every remaining `getX(id)` method.
    json!({ "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"] })
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
            "description": "Solana-style JSON-RPC 2.0 surface over every OpenFiat domain: advertisements, reservations, settlement, trade, disputes, identity, reputation, governance, service providers, notifications, oracles, risk, snapshots, and sessions. POST to /rpc with {\"jsonrpc\":\"2.0\",\"id\":...,\"method\":...,\"params\":{...}}.",
        },
        "servers": [{ "name": "this node", "url": "/rpc" }],
        "methods": methods,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_dispatch_table_method_appears_in_the_document() {
        let table = build_table::<MemoryStore>();
        let document = build_document();
        let documented: Vec<&str> = document["methods"].as_array().unwrap().iter().map(|m| m["name"].as_str().unwrap()).collect();
        for name in table.method_names() {
            assert!(documented.contains(&name), "{name} is dispatchable but not documented");
        }
        assert_eq!(documented.len(), table.method_names().len(), "documented method count must match the dispatch table exactly — no orphaned entries");
    }

    #[test]
    fn send_methods_document_the_base64_payload_shape() {
        let document = build_document();
        let send_method = document["methods"].as_array().unwrap().iter().find(|m| m["name"] == "sendAdvertisementCreate").unwrap();
        assert!(send_method["params"][0]["schema"]["properties"]["data"].is_object());
    }
}
