//! Event ID computation (OGP §5): "deterministically generated from Event
//! Type, Payload, Timestamp, Sender Identity, Digital Signature.
//! Duplicate Event IDs MUST represent identical events."
//!
//! Ed25519 signatures are themselves deterministic (RFC 8032 — no random
//! nonce), so hashing the signature alongside the content it covers is
//! still reproducible: the same origin signing the same content always
//! produces the same signature, and therefore the same Event ID.

use openfiat_crypto::sha256;
use openfiat_types::{EventId, EventType, PeerId, Signature, Timestamp};

/// The bytes an origin signs, and every recipient re-derives to verify —
/// everything in the envelope except the signature itself.
pub fn signable_bytes(
    event_type: &EventType,
    ofs_spec: u16,
    origin: &PeerId,
    timestamp: Timestamp,
    payload: &[u8],
) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(
        event_type.as_str().len() + 2 + origin.as_bytes().len() + 8 + payload.len(),
    );
    bytes.extend_from_slice(event_type.as_str().as_bytes());
    bytes.extend_from_slice(&ofs_spec.to_be_bytes());
    bytes.extend_from_slice(origin.as_bytes());
    bytes.extend_from_slice(&timestamp.as_millis().to_be_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

/// Compute the Event ID for a not-yet-assembled event.
pub fn compute(
    event_type: &EventType,
    payload: &[u8],
    timestamp: Timestamp,
    origin: &PeerId,
    signature: &Signature,
) -> EventId {
    let mut input = Vec::with_capacity(
        event_type.as_str().len() + payload.len() + 8 + origin.as_bytes().len() + 64,
    );
    input.extend_from_slice(event_type.as_str().as_bytes());
    input.extend_from_slice(payload);
    input.extend_from_slice(&timestamp.as_millis().to_be_bytes());
    input.extend_from_slice(origin.as_bytes());
    if let Some(signature_bytes) = signature.as_bytes() {
        input.extend_from_slice(&signature_bytes);
    }
    EventId::from_bytes(sha256(&input))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signature() -> Signature {
        Signature::from_bytes([7u8; 64])
    }

    #[test]
    fn is_deterministic_for_identical_inputs() {
        let event_type = EventType::new("AdvertisementCreated").unwrap();
        let origin = PeerId::from_bytes(vec![1, 2, 3]);
        let ts = Timestamp::from_millis(1_000);
        let a = compute(&event_type, b"payload", ts, &origin, &signature());
        let b = compute(&event_type, b"payload", ts, &origin, &signature());
        assert_eq!(a, b);
    }

    #[test]
    fn differs_when_the_payload_differs() {
        let event_type = EventType::new("AdvertisementCreated").unwrap();
        let origin = PeerId::from_bytes(vec![1, 2, 3]);
        let ts = Timestamp::from_millis(1_000);
        let a = compute(&event_type, b"payload-a", ts, &origin, &signature());
        let b = compute(&event_type, b"payload-b", ts, &origin, &signature());
        assert_ne!(a, b);
    }
}
