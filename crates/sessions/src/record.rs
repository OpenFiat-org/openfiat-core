//! The session shape (OFS-1400 §6-7, §20).

use openfiat_types::{PeerId, PublicKey, Timestamp};

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct SessionId(String);

impl SessionId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// §7's field list. §20: a wallet may hold several of these
/// concurrently (desktop, mobile, merchant terminal, API client), each
/// with its own `SessionId` — revoking one MUST NOT affect the others,
/// which falls out naturally here since each is its own keyed record.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub wallet: PeerId,
    pub wallet_public_key: PublicKey,
    pub authenticated_at: Timestamp,
    pub expires_at: Timestamp,
    /// "Connected Client" (§7) — free text, e.g. `"web"`, `"mobile"`,
    /// `"merchant-terminal"`, `"api-client"`.
    pub client: String,
    /// "Current Node" (§7) — the Primary Session Host (§11) currently
    /// servicing this session.
    pub host_node: PeerId,
    /// "Supported Permissions" (§7) — no other spec enumerates a fixed
    /// permission vocabulary, so this stays a free-form capability list
    /// rather than a closed enum invented here.
    pub permissions: Vec<String>,
    /// §13/§18: bumped on every renewal/migration; deterministic
    /// version ordering is how conflicting replicas resolve.
    pub version: u64,
    pub revoked: bool,
}

impl Session {
    /// §14: "Expired sessions MUST NOT be accepted" — also false once
    /// revoked (§16).
    pub fn is_current(&self, now: Timestamp) -> bool {
        !self.revoked && now.as_millis() < self.expires_at.as_millis()
    }
}
