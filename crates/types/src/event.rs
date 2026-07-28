//! The gossip event envelope shared by every event on the network (OGP §6).
//!
//! Concrete event *names* (`AdvertisementCreated`, `ReservationOpened`, ...)
//! are owned by the domain crate that emits them (per OFS-8100, the Event
//! Type Registry) — cataloguing all ~150 registry names here, before any
//! producing crate exists, would be dead code. [`EventType`] only validates
//! that a name has the registry's required shape (PascalCase, a completed
//! state transition's name) and carries it as an interned string.

use crate::identity::{PeerId, Signature};
use crate::priority::Priority;
use crate::timestamp::Timestamp;

/// A content-derived event identifier.
///
/// Computed by hashing the envelope's signed contents (`openfiat-crypto`'s
/// job) so two nodes that independently receive the same event always
/// agree on its ID — this is what makes gossip's duplicate-detection work
/// without a central sequence authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct EventId([u8; 32]);

impl EventId {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// An event name from the OFS-8100 Event Type Registry (e.g. `"AdvertisementCreated"`).
///
/// Validated for shape only — the registry itself, and the authority to
/// originate a given name, belong to the emitting domain crate (OGP §7:
/// "Node implementations MUST reject unauthorized event types").
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct EventType(String);

/// An event name failed OFS-8100's naming rule: non-empty and PascalCase
/// (starts with an uppercase ASCII letter, contains only ASCII alphanumerics).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidEventType(pub String);

impl EventType {
    /// Validate and wrap an event name.
    pub fn new(name: impl Into<String>) -> Result<Self, InvalidEventType> {
        let name = name.into();
        let is_pascal_case = name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase())
            && name.chars().all(|c| c.is_ascii_alphanumeric());
        if is_pascal_case {
            Ok(Self(name))
        } else {
            Err(InvalidEventType(name))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The common transport envelope every gossip event travels in (OGP §6).
///
/// `payload`'s shape is defined by whichever OFS specification `event_type`
/// belongs to — this crate has no opinion on it beyond "bytes to be
/// deserialized once the type is known".
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EventEnvelope {
    pub id: EventId,
    pub event_type: EventType,
    /// The OFS spec number defining `event_type`'s payload (e.g. `2100` for OAP).
    pub ofs_spec: u16,
    pub version: u16,
    pub origin: PeerId,
    pub timestamp: Timestamp,
    pub ttl: u8,
    pub priority: Priority,
    pub signature: Signature,
    pub payload: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_pascal_case_names() {
        assert!(EventType::new("AdvertisementCreated").is_ok());
    }

    #[test]
    fn rejects_non_pascal_case_names() {
        assert!(EventType::new("advertisementCreated").is_err());
        assert!(EventType::new("").is_err());
        assert!(EventType::new("Advertisement_Created").is_err());
    }
}
