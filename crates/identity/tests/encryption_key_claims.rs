//! Two wallets exchange a secret through their published encryption keys,
//! and a third party holding the same replica reads nothing.
//!
//! This is the fix stated as a test. Before it, a `KeyGrant` was sealed to
//! the recipient's Ed25519 wallet key, and opening one needed that key's
//! secret scalar — which a browser wallet does not expose, so a real user
//! could not open a grant addressed to them and the confidential trade
//! channel was unusable between two ordinary people.
//!
//! Nothing here is trade-channel-specific on purpose: a grant is an
//! `openfiat_crypto::SealedBox` and nothing more, so what has to hold is
//! that a wallet can publish a key, a counterparty can find it, and only
//! that wallet can open what was sealed to it. Everything the trade
//! channel adds sits on top of exactly this.

use openfiat_crypto::{DERIVATION_MESSAGE, EncryptionKeypair, Keypair, SealError};
use openfiat_identity::events::{ClaimPublish, SignedClaimPublish};
use openfiat_identity::{ClaimId, ClaimType, IdentityRegistry};
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_storage::mem::MemoryStore;
use openfiat_types::{PeerId, Timestamp};

/// A wallet, and the encryption key it derives from its own signature over
/// the one fixed message. No randomness anywhere: the same wallet on a new
/// machine reaches the same key, which is the whole reason the key is
/// derived rather than generated.
struct Wallet {
    keypair: Keypair,
    encryption: EncryptionKeypair,
}

impl Wallet {
    fn new(seed: u8) -> Self {
        let keypair = Keypair::from_seed([seed; 32]);
        let encryption =
            EncryptionKeypair::from_wallet_signature(&keypair.sign(DERIVATION_MESSAGE.as_bytes()))
                .expect("the wallet's own signature is 64 bytes");
        Self {
            keypair,
            encryption,
        }
    }

    fn peer_id(&self) -> PeerId {
        peer_id_from_public_key(&self.keypair.public_key()).unwrap()
    }

    /// Publish the public half as an OFS-5000 claim, exactly as a client
    /// does: signed by the wallet key, so nobody else can publish it.
    fn enrol(&self, registry: &IdentityRegistry<MemoryStore>, claim_id: &str) {
        registry
            .apply_publish(SignedClaimPublish::sign(
                ClaimPublish {
                    id: ClaimId::new(claim_id),
                    wallet: self.peer_id(),
                    wallet_public_key: self.keypair.public_key(),
                    claim_type: ClaimType::EncryptionKey,
                    value: self.encryption.public_key().to_string(),
                    verified: false,
                    supersedes: None,
                    expires_at: None,
                    timestamp: Timestamp::now(),
                },
                &self.keypair,
            ))
            .expect("a well-formed encryption key claim is accepted");
    }
}

#[test]
fn two_wallets_open_what_the_other_sealed_and_an_outsider_opens_neither() {
    let registry = IdentityRegistry::new(MemoryStore::new());
    let buyer = Wallet::new(1);
    let seller = Wallet::new(2);
    let outsider = Wallet::new(3);

    buyer.enrol(&registry, "enc-buyer");
    seller.enrol(&registry, "enc-seller");
    outsider.enrol(&registry, "enc-outsider");

    // Each side looks the other up by peer id alone — the only thing they
    // know about each other before the trade — and seals to what they find.
    let now = Timestamp::now();
    let to_seller = registry
        .encryption_key(&seller.peer_id(), now)
        .expect("the seller enrolled")
        .seal(b"channel key for settlement-1")
        .unwrap();
    let to_buyer = registry
        .encryption_key(&buyer.peer_id(), now)
        .expect("the buyer enrolled")
        .seal(b"channel key for settlement-1")
        .unwrap();

    assert_eq!(
        seller.encryption.open(&to_seller).unwrap(),
        b"channel key for settlement-1",
        "the seller must open the grant the buyer addressed to them"
    );
    assert_eq!(
        buyer.encryption.open(&to_buyer).unwrap(),
        b"channel key for settlement-1",
        "the buyer must open the grant the seller addressed to them"
    );

    // The third party holds a full replica of both grants — that is what
    // gossip means — and neither opens.
    assert_eq!(outsider.encryption.open(&to_seller), Err(SealError::Failed));
    assert_eq!(outsider.encryption.open(&to_buyer), Err(SealError::Failed));

    // Nor does the *counterparty*: a grant is addressed to one reader, and
    // holding the same channel key does not make somebody else's copy of
    // it readable.
    assert_eq!(buyer.encryption.open(&to_seller), Err(SealError::Failed));
}

/// The recovery property, which is the reason for deriving rather than
/// generating: a client that has lost everything local re-derives the key
/// from a wallet signature and opens a grant sealed months earlier.
#[test]
fn a_client_that_lost_its_local_state_re_derives_the_key_and_opens_an_old_grant() {
    let registry = IdentityRegistry::new(MemoryStore::new());
    let wallet = Wallet::new(7);
    wallet.enrol(&registry, "enc-1");

    let grant = registry
        .encryption_key(&wallet.peer_id(), Timestamp::now())
        .unwrap()
        .seal(b"channel key")
        .unwrap();

    // A fresh browser on a different machine: nothing but the wallet.
    let restored = Wallet::new(7);
    assert_eq!(restored.encryption.open(&grant).unwrap(), b"channel key");
}

/// Nobody can publish an encryption key on somebody else's behalf, which
/// is what makes "seal to the key the registry returns" safe. Without this
/// the lookup would be an invitation: publish a claim naming the seller's
/// peer id, carry your own key, and read their grants.
#[test]
fn a_wallet_cannot_publish_an_encryption_key_for_another_wallet() {
    let registry = IdentityRegistry::new(MemoryStore::new());
    let victim = Wallet::new(1);
    let attacker = Wallet::new(2);

    let forged = SignedClaimPublish::sign(
        ClaimPublish {
            id: ClaimId::new("enc-forged"),
            // The victim's peer id, the attacker's key, the attacker's
            // signature. Every combination of those three is refused.
            wallet: victim.peer_id(),
            wallet_public_key: attacker.keypair.public_key(),
            claim_type: ClaimType::EncryptionKey,
            value: attacker.encryption.public_key().to_string(),
            verified: false,
            supersedes: None,
            expires_at: None,
            timestamp: Timestamp::now(),
        },
        &attacker.keypair,
    );

    assert!(
        registry.apply_publish(forged).is_err(),
        "a claim whose wallet does not derive from its own public key must be refused"
    );
    assert_eq!(
        registry.encryption_key(&victim.peer_id(), Timestamp::now()),
        None,
        "the victim must still read as not enrolled, not as enrolled under the attacker's key"
    );
}
