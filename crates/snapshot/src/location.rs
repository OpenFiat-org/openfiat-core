//! Where a snapshot's bytes can actually be fetched from — the one thing
//! OFS-1300 §8's metadata list never had, and without which a joining
//! node learns a snapshot *exists* and can verify a hash it has no way
//! to obtain.
//!
//! **Why plain HTTP from an arbitrary host is fine here.** A location is
//! not a trust statement. `SnapshotMetadata::state_root` is a digest of
//! the uncompressed state and `size_bytes` bounds the download; both are
//! checked before a single byte reaches the store
//! (`SnapshotIndex::import`). A mirror that serves the wrong bytes fails
//! verification and is rejected — it cannot make a node believe anything.
//! So the transport needs no confidentiality and no server authentication,
//! and requiring TLS would only restrict who is able to mirror. That is
//! the same reasoning behind Solana's own unauthenticated snapshot
//! fetches, and it is why an operator can put snapshots behind any CDN,
//! bucket, or spare box without the protocol caring.
//!
//! What a location *can* do is decide who gets asked. That is why the URL
//! travels inside the signed announcement (see [`crate::events`]) and why
//! it is validated to an absolute `http`/`https` URL here: a
//! deserialization that accepts `file:///etc/shadow` turns every fetching
//! node into a local-file reader for whoever put the announcement on the
//! wire.

use crate::error::SnapshotError;
use std::fmt;

/// Generous next to any real URL, tight enough that an announcement
/// cannot be used as a kilobyte-per-location gossip amplifier.
const MAX_LOCATION_LEN: usize = 2048;

/// An absolute `http`/`https` URL a snapshot's compressed bytes can be
/// downloaded from. Constructed only through validation — including on
/// the deserialization path (`serde(try_from)`), so a peer cannot gossip
/// an announcement carrying a scheme this node would act on.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SnapshotLocation(String);

impl SnapshotLocation {
    /// The only constructor. Rejects, in order: oversize input, anything
    /// that is not `http://`/`https://`, an empty authority, embedded
    /// credentials, and any whitespace or control character.
    pub fn parse(raw: impl Into<String>) -> Result<Self, SnapshotError> {
        let raw: String = raw.into();
        if raw.is_empty() || raw.len() > MAX_LOCATION_LEN {
            return Err(SnapshotError::InvalidLocation);
        }
        if raw
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || !c.is_ascii())
        {
            return Err(SnapshotError::InvalidLocation);
        }

        let rest = raw
            .strip_prefix("https://")
            .or_else(|| raw.strip_prefix("http://"))
            .ok_or(SnapshotError::InvalidLocation)?;
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        if authority.is_empty() {
            return Err(SnapshotError::InvalidLocation);
        }
        // `http://user:token@host/...` in a *signed, gossiped* record is
        // either a leaked credential or bait pointing at a host that is
        // not the one a reader's eye lands on. Neither is worth supporting.
        if authority.contains('@') {
            return Err(SnapshotError::InvalidLocation);
        }
        Ok(Self(raw))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Appends a path segment, normalizing the single slash between them
    /// — how a producer turns its configured public base URL into the
    /// per-snapshot URL it announces.
    pub fn join(&self, segment: &str) -> Result<Self, SnapshotError> {
        Self::parse(format!(
            "{}/{}",
            self.0.trim_end_matches('/'),
            segment.trim_start_matches('/')
        ))
    }
}

impl fmt::Display for SnapshotLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for SnapshotLocation {
    type Error = SnapshotError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::parse(raw)
    }
}

impl From<SnapshotLocation> for String {
    fn from(location: SnapshotLocation) -> Self {
        location.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_http_and_https() {
        assert!(SnapshotLocation::parse("http://10.0.0.7:7080/snapshot/snap-1").is_ok());
        assert!(SnapshotLocation::parse("https://archive.example/snap-1.snapshot").is_ok());
    }

    #[test]
    fn rejects_every_scheme_that_is_not_http() {
        for raw in [
            "file:///etc/shadow",
            "ftp://host/x",
            "data:text/plain,hi",
            "/snapshot/snap-1",
            "archive.example/snap-1",
        ] {
            assert_eq!(
                SnapshotLocation::parse(raw),
                Err(SnapshotError::InvalidLocation),
                "{raw} must not parse"
            );
        }
    }

    #[test]
    fn rejects_embedded_credentials_and_empty_authorities() {
        assert_eq!(
            SnapshotLocation::parse("http://user:token@evil.example/snap"),
            Err(SnapshotError::InvalidLocation)
        );
        assert_eq!(
            SnapshotLocation::parse("http:///snap"),
            Err(SnapshotError::InvalidLocation)
        );
    }

    #[test]
    fn rejects_control_characters_and_oversize_input() {
        assert_eq!(
            SnapshotLocation::parse("http://host/a\nb"),
            Err(SnapshotError::InvalidLocation)
        );
        assert_eq!(
            SnapshotLocation::parse(format!("http://host/{}", "a".repeat(MAX_LOCATION_LEN))),
            Err(SnapshotError::InvalidLocation)
        );
    }

    /// The validation has to hold on the *deserialization* path, not just
    /// at construction — that is the path a gossiped announcement takes.
    #[test]
    fn deserializing_a_bad_scheme_fails_rather_than_producing_a_location() {
        let result =
            openfiat_serialization::json::from_str::<SnapshotLocation>("\"file:///etc/shadow\"");
        assert!(result.is_err());
    }

    #[test]
    fn join_normalizes_the_slash() {
        let base = SnapshotLocation::parse("http://host:7080/").unwrap();
        assert_eq!(
            base.join("/snapshot/snap-1").unwrap().as_str(),
            "http://host:7080/snapshot/snap-1"
        );
    }
}
