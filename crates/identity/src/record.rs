//! The identity claim shape (OFS-5000 §6-8).
//!
//! Real OTP delivery/verification (§9 — email/SMS/Telegram/Discord/X
//! account-ownership proof) needs a real verification-provider
//! integration (SMS/email gateways, Phase 6c territory) this crate
//! doesn't have yet. This crate defines the claim lifecycle and signed
//! events; whether a contact claim is actually `SelfAttested` at publish
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
    /// A profile picture, whose `value` is an IPFS CID rather than an
    /// image or a URL.
    ///
    /// A CID because the alternatives are worse in specific ways. Image
    /// bytes in the claim would put an avatar into every node's gossip
    /// log and replay it forever. A URL would let the owner change what
    /// the picture is after publication, silently, for everyone — an
    /// immutable claim pointing at mutable content is not immutable, and
    /// it would also let an avatar be a tracking beacon that reports
    /// every viewer to a server the owner controls. A CID names one
    /// specific image; changing it means publishing a new claim with
    /// `supersedes` set, which is exactly the visible history §11 asks
    /// for.
    Avatar,
    /// The X25519 public key other people seal things *to* this wallet
    /// with, base58. See `openfiat_crypto::encryption_key`.
    ///
    /// # Why identity is the right home for it
    ///
    /// A confidential trade channel distributes its content key as one
    /// `openfiat_crypto::SealedBox` per reader. Sealing is addressed to a
    /// key, and until now that key was the recipient's Ed25519 wallet key
    /// — which a browser wallet will never hand out the secret to, so the
    /// recipient could not open their own grant. The whole feature was
    /// unusable between two ordinary users.
    ///
    /// What was missing was a *published* key the recipient can prove they
    /// hold. That is a claim: a signed, gossiped, revocable, supersedable
    /// assertion binding a value to a wallet, which is exactly this
    /// crate's job. The binding is the part that matters — `verify()`
    /// refuses a claim whose `wallet` does not derive from its own
    /// `wallet_public_key`, so an encryption key can only ever be
    /// published by the wallet it belongs to, and a counterparty sealing
    /// to it is sealing to a key nobody else could have named.
    ///
    /// # This one is not self-asserted, unlike every other type
    ///
    /// A `MerchantName` may be anything; a wrong one misleads a reader. A
    /// wrong `EncryptionKey` is a live cryptographic failure — a grant
    /// sealed to a small-order point is readable by anybody, and one
    /// sealed to a malformed value is readable by nobody. So this is the
    /// second type after [`ClaimType::Avatar`] whose value is validated at
    /// publication, and the first whose validation is a security check
    /// rather than a formatting one.
    ///
    /// A wallet may hold only one of these in force at a time in practice:
    /// rotating means publishing a new claim with `supersedes` set to the
    /// old one, and the old grants stay readable under the old key because
    /// the ciphertext is already replicated everywhere. Rotation limits
    /// future exposure and undoes none of the past, which is the same
    /// thing `KeyGrant` says about itself.
    EncryptionKey,
    Custom(String),
}

impl ClaimType {
    /// Whether `value` is acceptable for this claim type.
    ///
    /// Two types constrain their value, and both for the same reason: a
    /// consumer does something with it beyond displaying it. The contact
    /// types are self-asserted strings that OFS-5000 deliberately does not
    /// constrain, and inventing a format for them here would reject claims
    /// the specification allows.
    pub fn accepts(&self, value: &str) -> bool {
        match self {
            // Validated at publication rather than at render time so a
            // string that is not a CID never reaches storage, never
            // reaches gossip, and never reaches a viewer that might
            // concatenate it into a gateway URL. Checking only in the
            // interface would leave every other consumer — the explorer,
            // an SDK user, a future client — to rediscover the same
            // requirement or omit it.
            Self::Avatar => openfiat_crypto::Cid::parse(value).is_ok(),
            // Checked here, at the same point and for a sharper reason.
            // `parse` rejects a malformed key and, more importantly, a
            // small-order point: a grant sealed to one derives a shared
            // secret that is public knowledge, so the "sealed" channel
            // would be readable by every node holding a replica. A claim
            // carrying that must never reach the store, and refusing it at
            // render time would be far too late — the gossip has already
            // gone out and a counterparty has already sealed to it.
            Self::EncryptionKey => openfiat_crypto::EncryptionPublicKey::parse(value).is_ok(),
            _ => true,
        }
    }
}

/// A claim's verification status.
///
/// `SelfAttested` means the claiming wallet signed the claim itself — it is
/// NOT third-party verification, and a consumer must not treat it as such.
/// It is named `SelfAttested` (not `Verified`) precisely so a counterparty
/// cannot mistake a self-signed claim for an externally attested one.
///
/// A future `Verified` variant will require a signature from a distinct
/// verifier authority (not the claimant). It is deliberately NOT added yet:
/// there is no verifier-authority design or key on devnet, and an
/// unreachable `Verified` would reintroduce exactly the ambiguity this
/// rename removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum VerificationStatus {
    Unverified,
    SelfAttested,
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

/// The encryption key to seal to for the wallet `claims` belong to, or
/// `None` if it has published none that is still in force.
///
/// "In force" is resolved through `supersedes` and not by taking the
/// newest, because those are different questions and only the first is
/// what the record says. A wallet that rotated its key twice holds three
/// `EncryptionKey` claims and exactly one of them is unreplaced.
///
/// `None` is a real answer a caller must handle rather than paper over: it
/// means the counterparty has not enrolled yet, and sealing to *anything*
/// else — their wallet key, say — produces a grant they cannot open. That
/// is the failure this whole mechanism exists to end, so it is better
/// surfaced as "they have not opened this trade yet" than retried with a
/// key that will not work.
///
/// Ambiguity is resolved by taking the newest of whatever remains, so two
/// nodes reading the same claim set always seal to the same key. That case
/// only arises if a wallet published a second key without superseding the
/// first, which its own client should not do.
pub fn current_encryption_key(
    claims: &[Claim],
    now: Timestamp,
) -> Option<openfiat_crypto::EncryptionPublicKey> {
    let replaced: std::collections::HashSet<&ClaimId> = claims
        .iter()
        .filter_map(|c| c.supersedes.as_ref())
        .collect();
    claims
        .iter()
        .filter(|claim| {
            claim.claim_type == ClaimType::EncryptionKey
                && claim.is_valid(now)
                && !replaced.contains(&claim.id)
        })
        .max_by_key(|claim| claim.created_at.as_millis())
        .and_then(|claim| openfiat_crypto::EncryptionPublicKey::parse(&claim.value).ok())
}
