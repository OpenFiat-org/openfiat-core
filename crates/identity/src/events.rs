//! Signed identity events. `ClaimPublish` is self-consistency verified;
//! `ClaimVerify` and `ClaimRevoke` are verified against the claim's
//! on-file wallet key, the same two-tier pattern used everywhere else in
//! this workspace.

use crate::error::IdentityError;
use crate::record::{ClaimId, ClaimType};
use openfiat_crypto::{Keypair, verify};
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_types::{PeerId, PublicKey, Signature, Timestamp};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClaimPublish {
    pub id: ClaimId,
    pub wallet: PeerId,
    pub wallet_public_key: PublicKey,
    pub claim_type: ClaimType,
    pub value: String,
    /// Whether this claim is already verified at publish time — the
    /// caller's responsibility; see the `record` module doc.
    pub verified: bool,
    pub supersedes: Option<ClaimId>,
    pub expires_at: Option<Timestamp>,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedClaimPublish {
    pub publish: ClaimPublish,
    pub signature: Signature,
}

impl SignedClaimPublish {
    pub fn sign(publish: ClaimPublish, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::json::to_bytes(&publish)
            .expect("ClaimPublish always serializes");
        Self {
            signature: keypair.sign(&bytes),
            publish,
        }
    }

    pub fn verify(&self) -> Result<(), IdentityError> {
        let expected = peer_id_from_public_key(&self.publish.wallet_public_key)
            .map_err(|_| IdentityError::InvalidSignature)?;
        if expected != self.publish.wallet {
            return Err(IdentityError::Unauthorized);
        }
        let bytes = openfiat_serialization::json::to_bytes(&self.publish)
            .map_err(|_| IdentityError::MalformedClaim)?;
        verify(&self.publish.wallet_public_key, &bytes, &self.signature)
            .map_err(|_| IdentityError::InvalidSignature)
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClaimVerify {
    pub claim_id: ClaimId,
    pub wallet: PeerId,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedClaimVerify {
    pub verify: ClaimVerify,
    pub signature: Signature,
}

impl SignedClaimVerify {
    pub fn sign(verify: ClaimVerify, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::CLAIM_VERIFY,
            &verify,
        )
        .expect("ClaimVerify always serializes");
        Self {
            signature: keypair.sign(&bytes),
            verify,
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClaimRevoke {
    pub claim_id: ClaimId,
    pub wallet: PeerId,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedClaimRevoke {
    pub revoke: ClaimRevoke,
    pub signature: Signature,
}

impl SignedClaimRevoke {
    pub fn sign(revoke: ClaimRevoke, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::CLAIM_REVOKE,
            &revoke,
        )
        .expect("ClaimRevoke always serializes");
        Self {
            signature: keypair.sign(&bytes),
            revoke,
        }
    }
}
