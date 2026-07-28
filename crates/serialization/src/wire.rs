//! Internal Rust↔Rust wire format: `postcard` over the gossip envelope and
//! anywhere else compactness matters more than human readability (decision
//! log item 3 in the P2P networking plan — `bincode` was tried first and
//! rejected as unmaintained).

use openfiat_types::ErrorCode;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fmt;

/// Failed to encode a value to the wire format.
#[derive(Debug)]
pub struct EncodeError(postcard::Error);

/// Failed to decode a value from the wire format.
#[derive(Debug)]
pub struct DecodeError(postcard::Error);

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
        write!(f, "wire encode failed: {}", self.0)
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "wire decode failed: {}", self.0)
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

/// Encode a value to its compact wire representation.
pub fn to_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, EncodeError> {
    postcard::to_allocvec(value).map_err(EncodeError)
}

/// Decode a value from its compact wire representation.
pub fn from_bytes<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, DecodeError> {
    postcard::from_bytes(bytes).map_err(DecodeError)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfiat_types::{Priority, Timestamp};

    #[test]
    fn round_trips_a_timestamp() {
        let ts = Timestamp::from_millis(1_753_000_000_000);
        let bytes = to_bytes(&ts).unwrap();
        assert_eq!(from_bytes::<Timestamp>(&bytes).unwrap(), ts);
    }

    #[test]
    fn round_trips_a_priority() {
        let p = Priority::Governance;
        let bytes = to_bytes(&p).unwrap();
        assert_eq!(from_bytes::<Priority>(&bytes).unwrap(), p);
    }

    #[test]
    fn decode_failure_maps_to_the_deserialization_error_code() {
        let err = from_bytes::<Timestamp>(&[]).unwrap_err();
        assert_eq!(err.code(), ErrorCode::DeserializationError);
    }
}
