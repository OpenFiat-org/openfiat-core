//! HTTP/RPC boundary format: JSON, where human/cross-language readability
//! matters more than size (`crates/rpc`, `crates/api`, `explorer/api`).

use openfiat_types::ErrorCode;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fmt;

/// Failed to encode a value to JSON.
#[derive(Debug)]
pub struct EncodeError(serde_json::Error);

/// Failed to decode a value from JSON.
#[derive(Debug)]
pub struct DecodeError(serde_json::Error);

impl EncodeError {
    /// The OFS-8000 code this failure maps to (`SERIALIZATION_ERROR`, 9003).
    pub const fn code(&self) -> ErrorCode {
        ErrorCode::SerializationError
    }
}

impl DecodeError {
    /// The OFS-8000 code this failure maps to (`DESERIALIZATION_ERROR`, 9004).
    pub const fn code(&self) -> ErrorCode {
        ErrorCode::DeserializationError
    }
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "JSON encode failed: {}", self.0)
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "JSON decode failed: {}", self.0)
    }
}

impl std::error::Error for EncodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

impl std::error::Error for DecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

/// Encode a value as a JSON string.
pub fn to_string<T: Serialize>(value: &T) -> Result<String, EncodeError> {
    serde_json::to_string(value).map_err(EncodeError)
}

/// Decode a value from a JSON string.
pub fn from_str<T: DeserializeOwned>(s: &str) -> Result<T, DecodeError> {
    serde_json::from_str(s).map_err(DecodeError)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfiat_types::{Priority, Timestamp};

    #[test]
    fn round_trips_a_timestamp() {
        let ts = Timestamp::from_millis(1_753_000_000_000);
        let json = to_string(&ts).unwrap();
        assert_eq!(from_str::<Timestamp>(&json).unwrap(), ts);
    }

    #[test]
    fn round_trips_a_priority() {
        let p = Priority::Advertisement;
        let json = to_string(&p).unwrap();
        assert_eq!(from_str::<Priority>(&json).unwrap(), p);
    }

    #[test]
    fn decode_failure_maps_to_the_deserialization_error_code() {
        let err = from_str::<Timestamp>("not json").unwrap_err();
        assert_eq!(err.code(), ErrorCode::DeserializationError);
    }
}
