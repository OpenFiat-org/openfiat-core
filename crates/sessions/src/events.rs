//! Signed session events (§8: "Every session MUST be cryptographically
//! signed... Unsigned sessions MUST be rejected"). `SessionCreate` is
//! self-consistency verified; `SessionRenew`/`SessionRevoke`/
//! `SessionMigrate` are verified against the session's on-file wallet
//! key, the same two-tier pattern used everywhere else in this workspace.

use crate::error::SessionError;
use crate::record::SessionId;
use openfiat_crypto::{Keypair, verify};
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_types::{PeerId, PublicKey, Signature, Timestamp};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SessionCreate {
    pub id: SessionId,
    pub wallet: PeerId,
    pub wallet_public_key: PublicKey,
    pub client: String,
    pub host_node: PeerId,
    pub permissions: Vec<String>,
    pub timestamp: Timestamp,
    pub expires_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedSessionCreate {
    pub create: SessionCreate,
    pub signature: Signature,
}

impl SignedSessionCreate {
    pub fn sign(create: SessionCreate, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::wire::to_bytes(&create).expect("SessionCreate always serializes");
        Self { signature: keypair.sign(&bytes), create }
    }

    pub fn verify(&self) -> Result<(), SessionError> {
        let expected = peer_id_from_public_key(&self.create.wallet_public_key).map_err(|_| SessionError::InvalidSignature)?;
        if expected != self.create.wallet {
            return Err(SessionError::Unauthorized);
        }
        let bytes = openfiat_serialization::wire::to_bytes(&self.create).map_err(|_| SessionError::MalformedSession)?;
        verify(&self.create.wallet_public_key, &bytes, &self.signature).map_err(|_| SessionError::InvalidSignature)
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SessionRenew {
    pub session_id: SessionId,
    pub wallet: PeerId,
    pub new_expires_at: Timestamp,
    pub version: u64,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedSessionRenew {
    pub renew: SessionRenew,
    pub signature: Signature,
}

impl SignedSessionRenew {
    pub fn sign(renew: SessionRenew, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::wire::to_bytes(&renew).expect("SessionRenew always serializes");
        Self { signature: keypair.sign(&bytes), renew }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SessionRevoke {
    pub session_id: SessionId,
    pub wallet: PeerId,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedSessionRevoke {
    pub revoke: SessionRevoke,
    pub signature: Signature,
}

impl SignedSessionRevoke {
    pub fn sign(revoke: SessionRevoke, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::wire::to_bytes(&revoke).expect("SessionRevoke always serializes");
        Self { signature: keypair.sign(&bytes), revoke }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SessionMigrate {
    pub session_id: SessionId,
    pub wallet: PeerId,
    pub new_host_node: PeerId,
    pub version: u64,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedSessionMigrate {
    pub migrate: SessionMigrate,
    pub signature: Signature,
}

impl SignedSessionMigrate {
    pub fn sign(migrate: SessionMigrate, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::wire::to_bytes(&migrate).expect("SessionMigrate always serializes");
        Self { signature: keypair.sign(&bytes), migrate }
    }
}
