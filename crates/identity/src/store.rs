//! The replicated local identity index (§17, §20 — reference
//! implementations store claims in RocksDB, the same replicated-KvStore
//! shape used throughout this workspace).

use crate::error::IdentityError;
use crate::events::{SignedClaimPublish, SignedClaimRevoke, SignedClaimVerify};
use crate::protocol;
use crate::record::{Claim, ClaimId, VerificationStatus};
use openfiat_crypto::verify;
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{EventEnvelope, PeerId, Timestamp};

const COLUMN_FAMILY: &str = "identity_claims";

/// §13 anti-spam tuning parameter: the number of *live* claims one wallet
/// may hold at once. Only claims that genuinely add to that count are
/// gated by it — see `apply_publish`, which exempts SUPERSEDE (it replaces
/// a claim rather than adding one). High enough that no legitimate wallet
/// enrolling every claim type this crate knows, several times over, would
/// ever approach it; low enough that one signer cannot grow the replicated
/// index without bound.
const MAX_CLAIMS_PER_WALLET: usize = 64;

/// Retention window for dead claims (revoked, expired, or superseded)
/// before `prune` reclaims them. Mirrors `GOSSIP_LOG_RETENTION`
/// (`crates/rpc/src/actor.rs`) — the flat week every other prune sweep in
/// this workspace uses, chosen there to sit comfortably above the 24h
/// replay-protection window `docs/architecture.md` requires. §11 asks that
/// a claim's history stay archived rather than mutated in place; this is
/// how long "archived" lasts once a claim is no longer live, not how long
/// it is kept from the moment it was published.
const CLAIM_RETENTION: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 60 * 60);

pub struct IdentityRegistry<S> {
    store: S,
}

impl<S: KvStore> IdentityRegistry<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }

    pub fn get(&self, id: &ClaimId) -> Option<Claim> {
        let bytes = self
            .store
            .get(COLUMN_FAMILY, id.as_str().as_bytes())
            .ok()
            .flatten()?;
        wire::from_bytes(&bytes).ok()
    }

    fn put(&self, claim: &Claim) {
        if let Ok(bytes) = wire::to_bytes(claim) {
            let _ = self
                .store
                .put(COLUMN_FAMILY, claim.id.as_str().as_bytes(), &bytes);
        }
    }

    pub fn all(&self) -> Vec<Claim> {
        self.store
            .iter_prefix(COLUMN_FAMILY, &[])
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(_, value)| wire::from_bytes(&value).ok())
            .collect()
    }

    /// §13: every claim currently associated with a wallet.
    pub fn find_by_wallet(&self, wallet: &PeerId) -> Vec<Claim> {
        self.all()
            .into_iter()
            .filter(|claim| &claim.wallet == wallet)
            .collect()
    }

    /// The key to seal a `KeyGrant` to for `wallet`, or `None` if it has
    /// published none in force. See [`crate::current_encryption_key`] for
    /// why `None` must be surfaced rather than substituted.
    pub fn encryption_key(
        &self,
        wallet: &PeerId,
        now: openfiat_types::Timestamp,
    ) -> Option<openfiat_crypto::EncryptionPublicKey> {
        crate::current_encryption_key(&self.find_by_wallet(wallet), now)
    }

    /// The ids of `wallet`'s claims that are live right now: not revoked,
    /// not expired as of `now` (`Claim::is_valid`), and not superseded by
    /// another of the wallet's claims. Mirrors the "in force" logic
    /// `crate::current_encryption_key` applies per claim type, generalised
    /// across all of them.
    ///
    /// Doubles as the answer to "is `id` a claim this wallet can
    /// legitimately supersede right now" — `apply_publish` uses membership
    /// in this set both to size the per-wallet cap and to decide whether a
    /// `supersedes` target is genuine (this wallet's own, currently live)
    /// rather than dangling, foreign, or already dead. A claim id that
    /// fails that membership test was never a replacement in the first
    /// place, so treating it as an exemption would let a `supersedes`
    /// field alone buy a wallet past the cap.
    fn live_claim_ids(
        &self,
        wallet: &PeerId,
        now: Timestamp,
    ) -> std::collections::HashSet<ClaimId> {
        let claims = self.find_by_wallet(wallet);
        let superseded: std::collections::HashSet<ClaimId> = claims
            .iter()
            .filter_map(|claim| claim.supersedes.clone())
            .collect();
        claims
            .into_iter()
            .filter(|claim| claim.is_valid(now) && !superseded.contains(&claim.id))
            .map(|claim| claim.id)
            .collect()
    }

    pub fn apply_publish(&self, signed: SignedClaimPublish) -> Result<ClaimId, IdentityError> {
        signed.verify()?;
        let id = signed.publish.id.clone();
        if self.get(&id).is_some() {
            return Err(IdentityError::DuplicateClaimId);
        }
        // Applies to a gossiped claim exactly as to a locally submitted
        // one: a peer must not be able to introduce a claim this node
        // would have refused from its own user.
        if !signed.publish.claim_type.accepts(&signed.publish.value) {
            return Err(IdentityError::MalformedClaim);
        }
        // §13 anti-spam. Two things a naive version of this check gets
        // wrong, both exploitable at zero cost by an ordinary wallet:
        //
        // 1. Liveness must be judged against this node's own clock, not
        //    `publish.timestamp`. That field is signed by the publisher,
        //    not by anyone else, so a wallet can claim any timestamp it
        //    likes — including one far enough in the future to make an
        //    otherwise-live, `expires_at`-bearing claim of its own look
        //    expired to the check below, undercounting its own live set
        //    and buying room under the cap that was never actually free.
        //    `publish.timestamp` is still what gets stored as the claim's
        //    `created_at`/`updated_at` below; only this liveness check
        //    must run on trusted time.
        // 2. `supersedes.is_some()` is not by itself proof of a
        //    replacement. Only a target this same wallet actually holds
        //    live right now is one — a `supersedes` naming a claim that
        //    was never published, belongs to another wallet, or is
        //    already dead (revoked, expired, or itself superseded) frees
        //    no slot, so exempting it from the cap would let a wallet
        //    publish unboundedly by wearing a `supersedes` field on every
        //    claim.
        let now = Timestamp::now();
        let live = self.live_claim_ids(&signed.publish.wallet, now);
        let is_genuine_supersede = signed
            .publish
            .supersedes
            .as_ref()
            .is_some_and(|target| live.contains(target));
        if !is_genuine_supersede && live.len() >= MAX_CLAIMS_PER_WALLET {
            return Err(IdentityError::TooManyClaims);
        }
        let publish = signed.publish;
        self.put(&Claim {
            id: id.clone(),
            wallet: publish.wallet,
            wallet_public_key: publish.wallet_public_key,
            claim_type: publish.claim_type,
            value: publish.value,
            verification_status: if publish.verified {
                VerificationStatus::SelfAttested
            } else {
                VerificationStatus::Unverified
            },
            supersedes: publish.supersedes,
            expires_at: publish.expires_at,
            revoked: false,
            created_at: publish.timestamp,
            updated_at: publish.timestamp,
        });
        Ok(id)
    }

    /// §9-10: mark a claim verified — only the claim's own wallet may do
    /// this, and only while it hasn't been revoked.
    pub fn apply_verify(&self, signed: SignedClaimVerify) -> Result<(), IdentityError> {
        let mut claim = self
            .get(&signed.verify.claim_id)
            .ok_or(IdentityError::ClaimNotFound)?;
        if claim.wallet != signed.verify.wallet {
            return Err(IdentityError::Unauthorized);
        }
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::CLAIM_VERIFY,
            &signed.verify,
        )
        .map_err(|_| IdentityError::MalformedClaim)?;
        verify(&claim.wallet_public_key, &bytes, &signed.signature)
            .map_err(|_| IdentityError::InvalidSignature)?;
        if claim.revoked {
            return Err(IdentityError::InvalidClaimState);
        }

        claim.verification_status = VerificationStatus::SelfAttested;
        claim.updated_at = signed.verify.timestamp;
        self.put(&claim);
        Ok(())
    }

    /// §12: revocation is permanent — a revoked claim stays revoked.
    pub fn apply_revoke(&self, signed: SignedClaimRevoke) -> Result<(), IdentityError> {
        let mut claim = self
            .get(&signed.revoke.claim_id)
            .ok_or(IdentityError::ClaimNotFound)?;
        if claim.wallet != signed.revoke.wallet {
            return Err(IdentityError::Unauthorized);
        }
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::CLAIM_REVOKE,
            &signed.revoke,
        )
        .map_err(|_| IdentityError::MalformedClaim)?;
        verify(&claim.wallet_public_key, &bytes, &signed.signature)
            .map_err(|_| IdentityError::InvalidSignature)?;
        if claim.revoked {
            return Err(IdentityError::InvalidClaimState);
        }

        claim.revoked = true;
        claim.updated_at = signed.revoke.timestamp;
        self.put(&claim);
        Ok(())
    }

    pub fn apply_event(&self, event: &EventEnvelope) {
        if event.ofs_spec != protocol::OFS_SPEC {
            return;
        }
        match event.event_type.as_str() {
            protocol::EVENT_CREATED => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_publish(signed);
                }
            }
            protocol::EVENT_VERIFIED => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_verify(signed);
                }
            }
            protocol::EVENT_REVOKED => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_revoke(signed);
                }
            }
            _ => {}
        }
    }

    /// One prune sweep: reclaims claims that are dead — revoked, expired,
    /// or superseded by another of the wallet's claims — and have been
    /// dead for at least `CLAIM_RETENTION`. Bounds storage the way §11's
    /// "archived, not deleted" mandate cannot by itself, and keeps
    /// `live_claim_count` (and so the cap in `apply_publish`) counting a
    /// set that does not grow forever.
    ///
    /// "Dead for at least `CLAIM_RETENTION`" is measured from
    /// `Claim::updated_at`: a revoke sets it to the revocation time, and a
    /// claim nobody has acted on since publication keeps its publish time
    /// — so an expired-or-superseded claim nobody ever revoked ages out
    /// starting from when it was published, same as `EventStore::prune_before`
    /// measures every gossip event from its own timestamp.
    ///
    /// Returns the number of claims removed.
    pub fn prune(&self, now: Timestamp) -> usize {
        let claims = self.all();
        let superseded: std::collections::HashSet<ClaimId> = claims
            .iter()
            .filter_map(|claim| claim.supersedes.clone())
            .collect();
        let cutoff_millis = now
            .as_millis()
            .saturating_sub(CLAIM_RETENTION.as_millis() as u64);
        let mut dropped = 0;
        for claim in &claims {
            let dead = claim.revoked || !claim.is_valid(now) || superseded.contains(&claim.id);
            if dead
                && claim.updated_at.as_millis() <= cutoff_millis
                && self
                    .store
                    .delete(COLUMN_FAMILY, claim.id.as_str().as_bytes())
                    .is_ok()
            {
                dropped += 1;
            }
        }
        dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{ClaimRevoke, ClaimVerify};
    use crate::record::ClaimType;
    use openfiat_crypto::Keypair;
    use openfiat_network::identity::peer_id_from_public_key;
    use openfiat_storage::mem::MemoryStore;
    use openfiat_types::Timestamp;

    fn publish_claim(keypair: &Keypair, id: &str, verified: bool) -> crate::events::ClaimPublish {
        crate::events::ClaimPublish {
            id: ClaimId::new(id),
            wallet: peer_id_from_public_key(&keypair.public_key()).unwrap(),
            wallet_public_key: keypair.public_key(),
            claim_type: ClaimType::Email,
            value: "user@example.com".to_string(),
            verified,
            supersedes: None,
            expires_at: None,
            timestamp: Timestamp::now(),
        }
    }

    #[test]
    fn an_unverified_claim_is_stored_as_unverified() {
        let registry = IdentityRegistry::new(MemoryStore::new());
        let keypair = Keypair::generate();
        let id = registry
            .apply_publish(SignedClaimPublish::sign(
                publish_claim(&keypair, "claim-1", false),
                &keypair,
            ))
            .unwrap();
        assert_eq!(
            registry.get(&id).unwrap().verification_status,
            VerificationStatus::Unverified
        );
    }

    #[test]
    fn verifying_transitions_to_verified() {
        let registry = IdentityRegistry::new(MemoryStore::new());
        let keypair = Keypair::generate();
        let id = registry
            .apply_publish(SignedClaimPublish::sign(
                publish_claim(&keypair, "claim-1", false),
                &keypair,
            ))
            .unwrap();

        let wallet = peer_id_from_public_key(&keypair.public_key()).unwrap();
        let verify_event = ClaimVerify {
            claim_id: id.clone(),
            wallet,
            timestamp: Timestamp::now(),
        };
        registry
            .apply_verify(SignedClaimVerify::sign(verify_event, &keypair))
            .unwrap();
        assert_eq!(
            registry.get(&id).unwrap().verification_status,
            VerificationStatus::SelfAttested
        );
    }

    #[test]
    fn a_different_wallet_cannot_verify_someone_elses_claim() {
        let registry = IdentityRegistry::new(MemoryStore::new());
        let owner = Keypair::generate();
        let attacker = Keypair::generate();
        let id = registry
            .apply_publish(SignedClaimPublish::sign(
                publish_claim(&owner, "claim-1", false),
                &owner,
            ))
            .unwrap();

        let owner_wallet = peer_id_from_public_key(&owner.public_key()).unwrap();
        let verify_event = ClaimVerify {
            claim_id: id,
            wallet: owner_wallet,
            timestamp: Timestamp::now(),
        };
        let result = registry.apply_verify(SignedClaimVerify::sign(verify_event, &attacker));
        assert_eq!(result, Err(IdentityError::InvalidSignature));
    }

    #[test]
    fn revocation_is_permanent() {
        let registry = IdentityRegistry::new(MemoryStore::new());
        let keypair = Keypair::generate();
        let id = registry
            .apply_publish(SignedClaimPublish::sign(
                publish_claim(&keypair, "claim-1", true),
                &keypair,
            ))
            .unwrap();

        let wallet = peer_id_from_public_key(&keypair.public_key()).unwrap();
        let revoke = ClaimRevoke {
            claim_id: id.clone(),
            wallet: wallet.clone(),
            timestamp: Timestamp::now(),
        };
        registry
            .apply_revoke(SignedClaimRevoke::sign(revoke, &keypair))
            .unwrap();
        assert!(registry.get(&id).unwrap().revoked);
        assert!(!registry.get(&id).unwrap().is_valid(Timestamp::now()));

        // Revoking again is rejected, not silently re-applied.
        let revoke_again = ClaimRevoke {
            claim_id: id,
            wallet,
            timestamp: Timestamp::now(),
        };
        let result = registry.apply_revoke(SignedClaimRevoke::sign(revoke_again, &keypair));
        assert_eq!(result, Err(IdentityError::InvalidClaimState));
    }

    #[test]
    fn find_by_wallet_returns_every_claim_for_that_wallet() {
        let registry = IdentityRegistry::new(MemoryStore::new());
        let keypair = Keypair::generate();
        registry
            .apply_publish(SignedClaimPublish::sign(
                publish_claim(&keypair, "claim-1", true),
                &keypair,
            ))
            .unwrap();
        let mut second = publish_claim(&keypair, "claim-2", true);
        second.claim_type = ClaimType::Phone;
        second.value = "+254700000000".to_string();
        registry
            .apply_publish(SignedClaimPublish::sign(second, &keypair))
            .unwrap();

        let wallet = peer_id_from_public_key(&keypair.public_key()).unwrap();
        assert_eq!(registry.find_by_wallet(&wallet).len(), 2);
    }

    /// The real CID this workspace uploaded to Filebase; see
    /// `openfiat_crypto::cid` for how it was obtained.
    const AVATAR_CID: &str = "bafkreibdmq27skp3wnycoyoqcei47etyaulerpsegivlkfvyhjkw7ufjva";

    fn avatar_claim(keypair: &Keypair, id: &str, value: &str) -> crate::events::ClaimPublish {
        crate::events::ClaimPublish {
            claim_type: ClaimType::Avatar,
            value: value.to_string(),
            ..publish_claim(keypair, id, false)
        }
    }

    #[test]
    fn an_avatar_naming_a_real_cid_is_stored() {
        let registry = IdentityRegistry::new(MemoryStore::new());
        let keypair = Keypair::generate();
        let id = registry
            .apply_publish(SignedClaimPublish::sign(
                avatar_claim(&keypair, "avatar-1", AVATAR_CID),
                &keypair,
            ))
            .unwrap();
        assert_eq!(registry.get(&id).unwrap().value, AVATAR_CID);
    }

    #[test]
    fn an_avatar_that_is_a_url_or_a_path_is_refused() {
        let registry = IdentityRegistry::new(MemoryStore::new());
        let keypair = Keypair::generate();
        for hostile in [
            "https://tracker.example/pixel.png",
            "../../../etc/passwd",
            "javascript:alert(1)",
            "",
        ] {
            // Signed by the genuine owner of the wallet — the signature
            // was never what stopped this.
            let signed =
                SignedClaimPublish::sign(avatar_claim(&keypair, "avatar-x", hostile), &keypair);
            assert_eq!(
                registry.apply_publish(signed),
                Err(IdentityError::MalformedClaim),
                "{hostile:?} must not become an avatar"
            );
            assert!(registry.get(&ClaimId::new("avatar-x")).is_none());
        }
    }

    #[test]
    fn a_peer_cannot_gossip_in_an_avatar_this_node_would_have_refused() {
        // The check has to be in the registry, not in the RPC handler:
        // this path never touches one.
        let registry = IdentityRegistry::new(MemoryStore::new());
        let keypair = Keypair::generate();
        let signed = SignedClaimPublish::sign(
            avatar_claim(&keypair, "avatar-1", "https://tracker.example/pixel.png"),
            &keypair,
        );
        let envelope = openfiat_types::EventEnvelope {
            id: openfiat_types::EventId::from_bytes([3; 32]),
            event_type: openfiat_types::EventType::new(protocol::EVENT_CREATED).unwrap(),
            ofs_spec: protocol::OFS_SPEC,
            version: 1,
            origin: peer_id_from_public_key(&keypair.public_key()).unwrap(),
            timestamp: Timestamp::now(),
            ttl: 8,
            priority: openfiat_types::Priority::Reputation,
            signature: openfiat_types::Signature::from_bytes([0u8; 64]),
            payload: openfiat_serialization::wire::to_bytes(&signed).unwrap(),
        };
        registry.apply_event(&envelope);
        assert!(registry.get(&ClaimId::new("avatar-1")).is_none());
    }

    #[test]
    fn a_contact_claim_is_still_unconstrained() {
        // OFS-5000 does not constrain these, and inventing a format here
        // would reject claims the specification allows.
        let registry = IdentityRegistry::new(MemoryStore::new());
        let keypair = Keypair::generate();
        let mut claim = publish_claim(&keypair, "claim-1", false);
        claim.value = "anything at all".to_string();
        assert!(
            registry
                .apply_publish(SignedClaimPublish::sign(claim, &keypair))
                .is_ok()
        );
    }

    fn encryption_claim(
        keypair: &Keypair,
        id: &str,
        value: &str,
        supersedes: Option<&str>,
    ) -> crate::events::ClaimPublish {
        let mut claim = publish_claim(keypair, id, false);
        claim.claim_type = ClaimType::EncryptionKey;
        claim.value = value.to_string();
        claim.supersedes = supersedes.map(ClaimId::new);
        claim
    }

    /// The whole point: a wallet publishes the key its counterparties seal
    /// grants to, and any node can look it up by wallet alone.
    #[test]
    fn a_wallet_publishes_the_key_its_counterparties_seal_to() {
        let registry = IdentityRegistry::new(MemoryStore::new());
        let keypair = Keypair::generate();
        let derived = openfiat_crypto::EncryptionKeypair::from_wallet_signature(
            &keypair.sign(openfiat_crypto::DERIVATION_MESSAGE.as_bytes()),
        )
        .unwrap();
        registry
            .apply_publish(SignedClaimPublish::sign(
                encryption_claim(&keypair, "enc-1", &derived.public_key().to_string(), None),
                &keypair,
            ))
            .unwrap();

        let wallet = peer_id_from_public_key(&keypair.public_key()).unwrap();
        let published = registry.encryption_key(&wallet, Timestamp::now()).unwrap();
        assert_eq!(published, derived.public_key());

        let sealed = published.seal(b"channel key").unwrap();
        assert_eq!(derived.open(&sealed).unwrap(), b"channel key");
    }

    /// A malformed key is refused at publication, not at use. By the time
    /// a counterparty tries to seal to it the claim has already been
    /// gossiped to every node, and a grant sealed to nonsense is one the
    /// recipient silently cannot open.
    #[test]
    fn a_malformed_encryption_key_never_reaches_the_store() {
        let registry = IdentityRegistry::new(MemoryStore::new());
        let keypair = Keypair::generate();
        assert_eq!(
            registry.apply_publish(SignedClaimPublish::sign(
                encryption_claim(&keypair, "enc-1", "user@example.com", None),
                &keypair,
            )),
            Err(IdentityError::MalformedClaim)
        );
    }

    /// The security check, and the reason this type validates at all: a
    /// grant sealed to a small-order point has a shared secret anybody can
    /// compute, so the channel would be "sealed" and world-readable.
    #[test]
    fn a_small_order_encryption_key_never_reaches_the_store() {
        let registry = IdentityRegistry::new(MemoryStore::new());
        let keypair = Keypair::generate();
        assert_eq!(
            registry.apply_publish(SignedClaimPublish::sign(
                // Base58 of 32 zero bytes: the X25519 identity element,
                // which every ECDH collapses onto.
                encryption_claim(&keypair, "enc-1", &"1".repeat(32), None),
                &keypair,
            )),
            Err(IdentityError::MalformedClaim)
        );
    }

    #[test]
    fn a_rotated_key_replaces_the_one_it_supersedes() {
        let registry = IdentityRegistry::new(MemoryStore::new());
        let keypair = Keypair::generate();
        let old = openfiat_crypto::EncryptionKeypair::from_secret([9u8; 32]);
        let new = openfiat_crypto::EncryptionKeypair::from_secret([11u8; 32]);
        registry
            .apply_publish(SignedClaimPublish::sign(
                encryption_claim(&keypair, "enc-1", &old.public_key().to_string(), None),
                &keypair,
            ))
            .unwrap();
        registry
            .apply_publish(SignedClaimPublish::sign(
                encryption_claim(
                    &keypair,
                    "enc-2",
                    &new.public_key().to_string(),
                    Some("enc-1"),
                ),
                &keypair,
            ))
            .unwrap();

        let wallet = peer_id_from_public_key(&keypair.public_key()).unwrap();
        assert_eq!(
            registry.encryption_key(&wallet, Timestamp::now()),
            Some(new.public_key()),
            "the superseded key must not still be what counterparties seal to"
        );
    }

    #[test]
    fn a_revoked_key_is_no_longer_what_anyone_seals_to() {
        let registry = IdentityRegistry::new(MemoryStore::new());
        let keypair = Keypair::generate();
        let key = openfiat_crypto::EncryptionKeypair::from_secret([9u8; 32]);
        let id = registry
            .apply_publish(SignedClaimPublish::sign(
                encryption_claim(&keypair, "enc-1", &key.public_key().to_string(), None),
                &keypair,
            ))
            .unwrap();
        let wallet = peer_id_from_public_key(&keypair.public_key()).unwrap();
        registry
            .apply_revoke(SignedClaimRevoke::sign(
                ClaimRevoke {
                    claim_id: id,
                    wallet: wallet.clone(),
                    timestamp: Timestamp::now(),
                },
                &keypair,
            ))
            .unwrap();
        assert_eq!(registry.encryption_key(&wallet, Timestamp::now()), None);
    }

    /// A wallet that has never enrolled must read as "no key", never as
    /// some other key: a caller that substituted the wallet's Ed25519 key
    /// would produce exactly the unopenable grant this claim type exists
    /// to eliminate.
    #[test]
    fn a_wallet_that_never_enrolled_has_no_key() {
        let registry = IdentityRegistry::new(MemoryStore::new());
        let keypair = Keypair::generate();
        registry
            .apply_publish(SignedClaimPublish::sign(
                publish_claim(&keypair, "claim-1", false),
                &keypair,
            ))
            .unwrap();
        let wallet = peer_id_from_public_key(&keypair.public_key()).unwrap();
        assert_eq!(registry.encryption_key(&wallet, Timestamp::now()), None);
    }

    /// Regression guard for cross-type signature confusion between
    /// `ClaimVerify` and `ClaimRevoke`.
    ///
    /// Both are signed as bare `serde_json` bytes of the inner struct, and
    /// both structs are `{claim_id, wallet, timestamp}` — identical field
    /// names and types. So the signed bytes are byte-for-byte identical for
    /// the same values, and a signature made to *verify* a claim is a valid
    /// signature to *revoke* it. `apply_verify` and `apply_revoke` also share
    /// the same preconditions (claim exists, wallet matches, `!revoked`), so
    /// there is no state that admits one and rejects the other.
    ///
    /// A `SignedClaimVerify` is gossiped in the clear, so any observer could
    /// lift the signature and permanently revoke the claim it verified —
    /// revocation being one-way, that was unrecoverable. Closed by signing a
    /// domain-separated preimage (`openfiat_serialization::domain`), so the
    /// two statements no longer share bytes. This test is what stops the
    /// separation being dropped again.
    #[test]
    fn a_verify_signature_is_not_accepted_as_a_revocation() {
        let registry = IdentityRegistry::new(MemoryStore::new());
        let victim = Keypair::generate();
        let id = registry
            .apply_publish(SignedClaimPublish::sign(
                publish_claim(&victim, "claim-1", false),
                &victim,
            ))
            .unwrap();
        let wallet = peer_id_from_public_key(&victim.public_key()).unwrap();
        let timestamp = Timestamp::now();

        // The victim signs a verification, and only a verification.
        let signed_verify = SignedClaimVerify::sign(
            ClaimVerify {
                claim_id: id.clone(),
                wallet: wallet.clone(),
                timestamp,
            },
            &victim,
        );
        registry.apply_verify(signed_verify.clone()).unwrap();
        assert_eq!(
            registry.get(&id).unwrap().verification_status,
            VerificationStatus::SelfAttested
        );

        // An attacker who never held the victim's key re-wraps the *same*
        // signature over the *same* bytes as a revocation.
        let forged = SignedClaimRevoke {
            revoke: ClaimRevoke {
                claim_id: id.clone(),
                wallet,
                timestamp,
            },
            signature: signed_verify.signature,
        };

        assert!(
            matches!(
                registry.apply_revoke(forged),
                Err(IdentityError::InvalidSignature)
            ),
            "a signature made to verify a claim must not also revoke it"
        );
        assert!(
            !registry.get(&id).unwrap().revoked,
            "the claim must survive a signature its owner never made for that purpose"
        );
    }

    #[test]
    fn the_cap_th_new_claim_succeeds_and_the_next_one_is_rejected() {
        let registry = IdentityRegistry::new(MemoryStore::new());
        let keypair = Keypair::generate();
        for i in 0..MAX_CLAIMS_PER_WALLET {
            registry
                .apply_publish(SignedClaimPublish::sign(
                    publish_claim(&keypair, &format!("claim-{i}"), false),
                    &keypair,
                ))
                .unwrap();
        }
        let wallet = peer_id_from_public_key(&keypair.public_key()).unwrap();
        assert_eq!(
            registry.find_by_wallet(&wallet).len(),
            MAX_CLAIMS_PER_WALLET
        );

        let result = registry.apply_publish(SignedClaimPublish::sign(
            publish_claim(&keypair, "claim-overflow", false),
            &keypair,
        ));
        assert_eq!(result, Err(IdentityError::TooManyClaims));
    }

    #[test]
    fn a_supersede_succeeds_even_at_the_cap() {
        let registry = IdentityRegistry::new(MemoryStore::new());
        let keypair = Keypair::generate();
        for i in 0..MAX_CLAIMS_PER_WALLET {
            registry
                .apply_publish(SignedClaimPublish::sign(
                    publish_claim(&keypair, &format!("claim-{i}"), false),
                    &keypair,
                ))
                .unwrap();
        }

        let mut supersede = publish_claim(&keypair, "claim-rotated", false);
        supersede.supersedes = Some(ClaimId::new("claim-0"));
        assert!(
            registry
                .apply_publish(SignedClaimPublish::sign(supersede, &keypair))
                .is_ok(),
            "a SUPERSEDE replaces a claim rather than adding one, so it must not be capped"
        );

        let wallet = peer_id_from_public_key(&keypair.public_key()).unwrap();
        assert_eq!(
            registry.find_by_wallet(&wallet).len(),
            MAX_CLAIMS_PER_WALLET + 1,
            "the superseded claim stays archived (§11), it is not deleted"
        );

        // Still at the live cap: a further genuinely-new claim is rejected.
        let result = registry.apply_publish(SignedClaimPublish::sign(
            publish_claim(&keypair, "claim-overflow", false),
            &keypair,
        ));
        assert_eq!(result, Err(IdentityError::TooManyClaims));
    }

    #[test]
    fn a_different_wallets_cap_is_independent() {
        let registry = IdentityRegistry::new(MemoryStore::new());
        let a = Keypair::generate();
        let b = Keypair::generate();
        for i in 0..MAX_CLAIMS_PER_WALLET {
            registry
                .apply_publish(SignedClaimPublish::sign(
                    publish_claim(&a, &format!("a-{i}"), false),
                    &a,
                ))
                .unwrap();
        }

        // `a` is at the cap; `b`, who has published nothing, is unaffected.
        assert!(
            registry
                .apply_publish(SignedClaimPublish::sign(
                    publish_claim(&b, "b-1", false),
                    &b
                ))
                .is_ok()
        );

        let overflow = registry.apply_publish(SignedClaimPublish::sign(
            publish_claim(&a, "a-overflow", false),
            &a,
        ));
        assert_eq!(overflow, Err(IdentityError::TooManyClaims));
    }

    #[test]
    fn prune_reclaims_dead_claims_past_the_retention_window_but_keeps_live_ones() {
        let registry = IdentityRegistry::new(MemoryStore::new());
        let keypair = Keypair::generate();
        let wallet = peer_id_from_public_key(&keypair.public_key()).unwrap();

        let old = Timestamp::from_millis(10_000_000_000);
        let retention_ms = CLAIM_RETENTION.as_millis() as u64;
        let past_retention = Timestamp::from_millis(old.as_millis() + retention_ms + 1);

        // Revoked, and old enough to prune.
        let mut revoked = publish_claim(&keypair, "revoked-1", false);
        revoked.timestamp = old;
        let revoked_id = registry
            .apply_publish(SignedClaimPublish::sign(revoked, &keypair))
            .unwrap();
        registry
            .apply_revoke(SignedClaimRevoke::sign(
                ClaimRevoke {
                    claim_id: revoked_id.clone(),
                    wallet: wallet.clone(),
                    timestamp: old,
                },
                &keypair,
            ))
            .unwrap();

        // Expired, and old enough to prune (never touched again after publish).
        let mut expired = publish_claim(&keypair, "expired-1", false);
        expired.timestamp = old;
        expired.expires_at = Some(Timestamp::from_millis(old.as_millis() + 1));
        let expired_id = registry
            .apply_publish(SignedClaimPublish::sign(expired, &keypair))
            .unwrap();

        // Superseded, and old enough to prune.
        let mut superseded = publish_claim(&keypair, "superseded-1", false);
        superseded.timestamp = old;
        let superseded_id = registry
            .apply_publish(SignedClaimPublish::sign(superseded, &keypair))
            .unwrap();
        let mut superseding = publish_claim(&keypair, "superseding-1", false);
        superseding.timestamp = old;
        superseding.supersedes = Some(superseded_id.clone());
        let superseding_id = registry
            .apply_publish(SignedClaimPublish::sign(superseding, &keypair))
            .unwrap();

        // A live claim: must survive.
        let mut live = publish_claim(&keypair, "live-1", false);
        live.timestamp = old;
        let live_id = registry
            .apply_publish(SignedClaimPublish::sign(live, &keypair))
            .unwrap();

        let dropped = registry.prune(past_retention);
        assert_eq!(
            dropped, 3,
            "revoked, expired, and superseded claims are removed"
        );

        assert!(registry.get(&revoked_id).is_none());
        assert!(registry.get(&expired_id).is_none());
        assert!(registry.get(&superseded_id).is_none());
        assert!(
            registry.get(&superseding_id).is_some(),
            "the claim that superseded another is itself live and must survive"
        );
        assert!(registry.get(&live_id).is_some());

        // Pruning again removes nothing further.
        assert_eq!(registry.prune(past_retention), 0);
    }

    #[test]
    fn after_prune_a_wallet_at_the_cap_can_publish_again() {
        let registry = IdentityRegistry::new(MemoryStore::new());
        let keypair = Keypair::generate();
        let wallet = peer_id_from_public_key(&keypair.public_key()).unwrap();

        let old = Timestamp::from_millis(10_000_000_000);
        let retention_ms = CLAIM_RETENTION.as_millis() as u64;
        let past_retention = Timestamp::from_millis(old.as_millis() + retention_ms + 1);

        for i in 0..MAX_CLAIMS_PER_WALLET {
            let mut claim = publish_claim(&keypair, &format!("cap-{i}"), false);
            claim.timestamp = old;
            registry
                .apply_publish(SignedClaimPublish::sign(claim, &keypair))
                .unwrap();
        }

        // At the cap: a genuinely-new claim is rejected.
        let mut overflow = publish_claim(&keypair, "cap-overflow", false);
        overflow.timestamp = old;
        assert_eq!(
            registry.apply_publish(SignedClaimPublish::sign(overflow, &keypair)),
            Err(IdentityError::TooManyClaims)
        );

        // Revoke one of the cap's claims and let it age past retention.
        registry
            .apply_revoke(SignedClaimRevoke::sign(
                ClaimRevoke {
                    claim_id: ClaimId::new("cap-0"),
                    wallet: wallet.clone(),
                    timestamp: old,
                },
                &keypair,
            ))
            .unwrap();
        assert_eq!(registry.prune(past_retention), 1);
        assert!(registry.get(&ClaimId::new("cap-0")).is_none());

        let mut refill = publish_claim(&keypair, "cap-refill", false);
        refill.timestamp = past_retention;
        assert!(
            registry
                .apply_publish(SignedClaimPublish::sign(refill, &keypair))
                .is_ok(),
            "revoking (and pruning) one of the cap's claims frees a slot"
        );
    }

    /// Regression guard: `supersedes.is_some()` alone must never exempt a
    /// publish from the cap. Only a target this same wallet actually holds
    /// live right now is a genuine replacement; a claim id that was never
    /// published, belongs to someone else, or was already dead frees no
    /// slot, and a version of the check that took the field at face value
    /// let a wallet at the cap publish without bound by wearing a
    /// `supersedes` on every claim.
    #[test]
    fn a_supersede_naming_a_fake_foreign_or_already_dead_claim_does_not_bypass_the_cap() {
        let registry = IdentityRegistry::new(MemoryStore::new());
        let keypair = Keypair::generate();
        let wallet = peer_id_from_public_key(&keypair.public_key()).unwrap();

        // A claim that is already dead before the wallet ever reaches the
        // cap — not one of the `MAX_CLAIMS_PER_WALLET` live claims below.
        let dead_id = registry
            .apply_publish(SignedClaimPublish::sign(
                publish_claim(&keypair, "already-dead", false),
                &keypair,
            ))
            .unwrap();
        registry
            .apply_revoke(SignedClaimRevoke::sign(
                ClaimRevoke {
                    claim_id: dead_id.clone(),
                    wallet: wallet.clone(),
                    timestamp: Timestamp::now(),
                },
                &keypair,
            ))
            .unwrap();

        // Fill the wallet to the live cap with genuinely new claims.
        for i in 0..MAX_CLAIMS_PER_WALLET {
            registry
                .apply_publish(SignedClaimPublish::sign(
                    publish_claim(&keypair, &format!("claim-{i}"), false),
                    &keypair,
                ))
                .unwrap();
        }

        // Repeated supersedes naming claim ids that were never published:
        // each must be rejected, not silently accumulate live claims.
        for n in 0..5 {
            let mut fake_target = publish_claim(&keypair, &format!("bypass-fake-{n}"), false);
            fake_target.supersedes = Some(ClaimId::new(format!("never-published-{n}")));
            assert_eq!(
                registry.apply_publish(SignedClaimPublish::sign(fake_target, &keypair)),
                Err(IdentityError::TooManyClaims),
                "a supersede naming a claim that was never published must not exempt the cap"
            );
        }

        // A supersede naming a claim that belongs to a different wallet.
        let other = Keypair::generate();
        let other_claim_id = registry
            .apply_publish(SignedClaimPublish::sign(
                publish_claim(&other, "other-wallets-claim", false),
                &other,
            ))
            .unwrap();
        let mut foreign_target = publish_claim(&keypair, "bypass-foreign", false);
        foreign_target.supersedes = Some(other_claim_id);
        assert_eq!(
            registry.apply_publish(SignedClaimPublish::sign(foreign_target, &keypair)),
            Err(IdentityError::TooManyClaims),
            "a supersede naming another wallet's claim must not exempt the cap"
        );

        // A supersede naming this wallet's own claim, but one that was
        // already dead before the wallet ever reached the cap — it frees
        // nothing now.
        let mut dead_target = publish_claim(&keypair, "bypass-dead", false);
        dead_target.supersedes = Some(dead_id);
        assert_eq!(
            registry.apply_publish(SignedClaimPublish::sign(dead_target, &keypair)),
            Err(IdentityError::TooManyClaims),
            "a supersede naming an already-dead claim must not exempt the cap"
        );

        // None of the rejected publishes above left a trace.
        assert_eq!(
            registry.find_by_wallet(&wallet).len(),
            MAX_CLAIMS_PER_WALLET + 1, // the live cap, plus the pre-revoked claim
            "every rejected publish above must have left no trace"
        );

        // A genuine supersede of one of the wallet's own live claims still
        // works at the cap — the fix must not have broken the exemption it
        // is supposed to preserve.
        let mut real_supersede = publish_claim(&keypair, "bypass-legit", false);
        real_supersede.supersedes = Some(ClaimId::new("claim-0"));
        assert!(
            registry
                .apply_publish(SignedClaimPublish::sign(real_supersede, &keypair))
                .is_ok(),
            "a genuine supersede of a live own claim must still be exempt"
        );
    }

    /// Regression guard: the cap must be decided on this node's own clock,
    /// never on the publisher-supplied `publish.timestamp`. A wallet
    /// holding an `expires_at`-bearing claim that is genuinely still valid
    /// could otherwise publish a new claim self-reporting a far-future
    /// timestamp, making that still-live claim look expired to the cap
    /// check and undercounting its own live set.
    #[test]
    fn a_forged_future_timestamp_does_not_bypass_the_cap() {
        let registry = IdentityRegistry::new(MemoryStore::new());
        let keypair = Keypair::generate();
        let wallet = peer_id_from_public_key(&keypair.public_key()).unwrap();
        let year_ms = 365 * 24 * 60 * 60 * 1000;

        // A claim that is genuinely still valid under the real clock.
        let mut expiring = publish_claim(&keypair, "expiring-claim", false);
        expiring.expires_at = Some(Timestamp::from_millis(
            Timestamp::now().as_millis() + year_ms,
        ));
        registry
            .apply_publish(SignedClaimPublish::sign(expiring, &keypair))
            .unwrap();

        // Fill the rest of the cap with ordinary claims.
        for i in 0..MAX_CLAIMS_PER_WALLET - 1 {
            registry
                .apply_publish(SignedClaimPublish::sign(
                    publish_claim(&keypair, &format!("claim-{i}"), false),
                    &keypair,
                ))
                .unwrap();
        }
        assert_eq!(
            registry.find_by_wallet(&wallet).len(),
            MAX_CLAIMS_PER_WALLET
        );

        // A genuinely new claim, self-reporting a timestamp a century in
        // the future. If the cap trusted `publish.timestamp`,
        // "expiring-claim" would read as expired and this would fit under
        // the cap; it must not.
        let mut forged = publish_claim(&keypair, "forged-future", false);
        forged.timestamp = Timestamp::from_millis(Timestamp::now().as_millis() + 100 * year_ms);
        assert_eq!(
            registry.apply_publish(SignedClaimPublish::sign(forged, &keypair)),
            Err(IdentityError::TooManyClaims),
            "the cap must be judged on this node's clock, not the publisher's self-reported timestamp"
        );
    }
}
