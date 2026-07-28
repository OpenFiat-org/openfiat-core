//! The signed snapshot announcement (OFS-1300 §12). Self-consistency
//! verified here (the announcer really is who it claims to be);
//! authorization against `openfiat-registry`'s on-file snapshot
//! providers happens at the store layer, the same two-tier split
//! `openfiat-oracles`/`openfiat-risk` use for their own providers.

use crate::error::SnapshotError;
use crate::record::SnapshotMetadata;
use openfiat_crypto::{Keypair, verify};
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_types::Signature;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedSnapshotAnnounce {
    pub metadata: SnapshotMetadata,
    pub signature: Signature,
}

impl SignedSnapshotAnnounce {
    pub fn sign(metadata: SnapshotMetadata, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::wire::to_bytes(&metadata).expect("SnapshotMetadata always serializes");
        Self { signature: keypair.sign(&bytes), metadata }
    }

    pub fn verify(&self) -> Result<(), SnapshotError> {
        let expected = peer_id_from_public_key(&self.metadata.producer_public_key).map_err(|_| SnapshotError::InvalidSignature)?;
        if expected != self.metadata.producer {
            return Err(SnapshotError::Unauthorized);
        }
        let bytes = openfiat_serialization::wire::to_bytes(&self.metadata).map_err(|_| SnapshotError::MalformedRecord)?;
        verify(&self.metadata.producer_public_key, &bytes, &self.signature).map_err(|_| SnapshotError::InvalidSignature)
    }
}
