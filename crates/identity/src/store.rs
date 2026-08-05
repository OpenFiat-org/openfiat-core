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
use openfiat_types::{EventEnvelope, PeerId};

const COLUMN_FAMILY: &str = "identity_claims";

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
        let publish = signed.publish;
        self.put(&Claim {
            id: id.clone(),
            wallet: publish.wallet,
            wallet_public_key: publish.wallet_public_key,
            claim_type: publish.claim_type,
            value: publish.value,
            verification_status: if publish.verified {
                VerificationStatus::Verified
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

        claim.verification_status = VerificationStatus::Verified;
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
            VerificationStatus::Verified
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
            VerificationStatus::Verified
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
}
