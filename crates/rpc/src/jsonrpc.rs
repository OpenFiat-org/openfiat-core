//! The JSON-RPC 2.0 envelope this crate's whole surface speaks —
//! deliberately modeled on Solana's own JSON-RPC HTTP API (one POST
//! endpoint, `getX`/`sendX` camelCase method names, an opaque pre-signed
//! payload handed to `sendSignedEvent` the same way Solana's
//! `sendTransaction` takes an opaque pre-signed transaction) rather than
//! a REST resource hierarchy.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcRequest {
    #[allow(dead_code)]
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Value,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(flatten)]
    pub outcome: Outcome,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum Outcome {
    Result { result: Value },
    Error { error: JsonRpcError },
}

impl JsonRpcResponse {
    pub fn success(id: Value, result: Value) -> Self {
        Self { jsonrpc: "2.0", id, outcome: Outcome::Result { result } }
    }

    pub fn failure(id: Value, error: JsonRpcError) -> Self {
        Self { jsonrpc: "2.0", id, outcome: Outcome::Error { error } }
    }
}

/// The standard JSON-RPC 2.0 error codes (`-32700..-32600`), plus a
/// single `-32000` "Application error" for every domain failure — OFS-
/// 8000's own numeric code and symbolic name go in `data` instead of
/// trying to cram ~70 domain codes into JSON-RPC's 100-slot reserved
/// server-error range (`-32099..-32000`).
#[derive(Debug, Clone, Serialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcError {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
    pub const APPLICATION_ERROR: i64 = -32000;

    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), data: None }
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_response_serializes_without_an_error_field() {
        let response = JsonRpcResponse::success(Value::from(1), Value::from("ok"));
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["result"], Value::from("ok"));
        assert!(json.get("error").is_none());
    }

    #[test]
    fn failure_response_serializes_without_a_result_field() {
        let response = JsonRpcResponse::failure(Value::from(1), JsonRpcError::new(JsonRpcError::METHOD_NOT_FOUND, "not found"));
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["error"]["code"], Value::from(JsonRpcError::METHOD_NOT_FOUND));
        assert!(json.get("result").is_none());
    }
}
