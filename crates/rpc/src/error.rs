//! The error type every method handler returns — converted to a
//! [`crate::jsonrpc::JsonRpcError`] at the response boundary.

use crate::jsonrpc::JsonRpcError;
use openfiat_types::ErrorCode;

#[derive(Debug, Clone)]
pub enum RpcError {
    MethodNotFound(String),
    InvalidParams(String),
    /// A domain crate's own typed error — carries OFS-8000's numeric
    /// code/name so callers get the same error vocabulary every other
    /// OpenFiat transport (REST, SDKs, CLI) uses.
    Application(ErrorCode),
    Internal(String),
}

impl RpcError {
    pub fn into_json_rpc_error(self) -> JsonRpcError {
        match self {
            Self::MethodNotFound(method) => JsonRpcError::new(
                JsonRpcError::METHOD_NOT_FOUND,
                format!("method not found: {method}"),
            ),
            Self::InvalidParams(reason) => JsonRpcError::new(JsonRpcError::INVALID_PARAMS, reason),
            // `ofsRetryable` carries OFS-8000 §16's judgement to the
            // caller instead of keeping it here.
            //
            // The registry has always known which failures a repeated
            // identical request can survive — `ErrorCode::retryable()`,
            // audited code by code, guarded by a test that fails if a new
            // code claims retryability without being written down. None
            // of it reached the wire: a client wanting to know whether to
            // back off or give up had to hardcode its own copy of the
            // table, in every language, and watch it drift from this one.
            // That duplication is the whole thing the flag exists to
            // prevent.
            //
            // Additive and safe for existing clients, which ignore
            // unknown `data` fields. `ofsErrorCode` stays the
            // authoritative identity (OFS-8200 §10); this is a derived
            // hint, and a client with a reason to disagree about a
            // specific code may still do so.
            Self::Application(code) => {
                JsonRpcError::new(JsonRpcError::APPLICATION_ERROR, code.name()).with_data(
                    serde_json::json!({
                        "ofsErrorCode": code.code(),
                        "ofsErrorName": code.name(),
                        "ofsRetryable": code.retryable(),
                    }),
                )
            }
            Self::Internal(reason) => JsonRpcError::new(JsonRpcError::INTERNAL_ERROR, reason),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_application_error_carries_the_ofs_8000_code_in_data() {
        let error = RpcError::Application(ErrorCode::AdvertisementNotFound).into_json_rpc_error();
        assert_eq!(error.code, JsonRpcError::APPLICATION_ERROR);
        assert_eq!(
            error.data.unwrap()["ofsErrorCode"],
            serde_json::Value::from(ErrorCode::AdvertisementNotFound.code())
        );
    }

    /// The registry's retryability judgement reaches the caller.
    ///
    /// Both directions, because the failure this guards against is a
    /// field that is always present and always the same — a `false`
    /// hardcoded at the boundary would pass a one-sided test and tell
    /// every client to give up on a timeout.
    #[test]
    fn an_application_error_says_whether_the_request_may_be_retried() {
        let transient = RpcError::Application(ErrorCode::OperationTimeout).into_json_rpc_error();
        assert_eq!(
            transient.data.unwrap()["ofsRetryable"],
            serde_json::Value::Bool(true)
        );

        let permanent =
            RpcError::Application(ErrorCode::SettlementAlreadyExists).into_json_rpc_error();
        assert_eq!(
            permanent.data.unwrap()["ofsRetryable"],
            serde_json::Value::Bool(false)
        );
    }

    /// Every code, not the two spot-checked above: the field is a
    /// projection of `ErrorCode::retryable()` and must not become a
    /// second, hand-maintained opinion about retryability.
    #[test]
    fn the_retryable_field_is_never_a_second_opinion() {
        for code in ErrorCode::ALL {
            let error = RpcError::Application(*code).into_json_rpc_error();
            assert_eq!(
                error.data.unwrap()["ofsRetryable"],
                serde_json::Value::Bool(code.retryable()),
                "{} disagrees with the registry",
                code.name()
            );
        }
    }
}
