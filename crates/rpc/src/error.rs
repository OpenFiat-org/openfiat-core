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
            Self::Application(code) => {
                JsonRpcError::new(JsonRpcError::APPLICATION_ERROR, code.name()).with_data(
                    serde_json::json!({ "ofsErrorCode": code.code(), "ofsErrorName": code.name() }),
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
}
