//! The identity claim shape (OFS-5000 §6-8).
//!
//! Real OTP delivery/verification (§9 — email/SMS/Telegram/Discord/X
//! account-ownership proof) needs a real verification-provider
//! integration (SMS/email gateways, Phase 6c territory) this crate
//! doesn't have yet. This crate defines the claim lifecycle and signed
//! events; whether a contact claim is actually `Verified` at publish
//! time is the caller's responsibility (the wallet application already
//! ran the OTP flow externally) — the same "off-chain step deferred,
//! on-chain/off-protocol integration comes later" pattern used by
//! `openfiat-settlement` (escrow release) and `openfiat-disputes` (stake
//! slashing).

use openfiat_types::{PeerId, PublicKey, Timestamp};

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ClaimId(String);

impl ClaimId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// §6/§8's examples. Level 1 contact types are OTP-verifiable; Level
/// 2/3 business/infrastructure fields are self-asserted (no OTP concept
/// applies), so `Custom` covers that open-ended set rather than
/// enumerating every example the spec names.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ClaimType {
    Email,
    Phone,
    Telegram,
    Discord,
    Twitter,
    MerchantName,
    BusinessName,
    Custom(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum VerificationStatus {
    Unverified,
    Verified,
}

/// §7/§11: claims are immutable after publication; an update publishes a
/// new claim with a new `ClaimId`, optionally linked to the one it
/// replaces via `supersedes` — the old claim remains archived (§11), not
/// mutated or deleted.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Claim {
    pub id: ClaimId,
    pub wallet: PeerId,
    pub wallet_public_key: PublicKey,
    pub claim_type: ClaimType,
    pub value: String,
    pub verification_status: VerificationStatus,
    pub supersedes: Option<ClaimId>,
    pub expires_at: Option<Timestamp>,
    pub revoked: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl Claim {
    /// §10: a claim consumers should actually trust right now — not
    /// revoked (§12) and not past its optional expiration.
    pub fn is_valid(&self, now: Timestamp) -> bool {
        !self.revoked
            && self
                .expires_at
                .is_none_or(|expiry| now.as_millis() < expiry.as_millis())
    }
}
