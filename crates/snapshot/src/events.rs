//! The signed snapshot announcement (OFS-1300 §12). Self-consistency
//! verified here (the announcer really is who it claims to be);
//! authorization against `openfiat-registry`'s on-file snapshot
//! providers happens at the store layer, the same two-tier split
//! `openfiat-oracles`/`openfiat-risk` use for their own providers.
//!
//! # Why `locations` is inside the signature
//!
//! [`SnapshotMetadata::locations`] was added to the *signed* payload, and
//! `verify` re-serializes that payload, so **every announcement signed
//! before this change now fails verification**. That is a breaking change
//! to a gossiped event. It is taken deliberately, on devnet, where the
//! entire announced population is one bring-up fixture that carried no
//! location and was therefore undownloadable anyway;
//! `protocol::SUPPORTED_PROTOCOL_VERSION` is bumped alongside it so an
//! old announcement is rejected as a version mismatch — a diagnosable
//! answer — instead of as a bad signature, which reads like an attack.
//!
//! The alternative considered was a separate *unsigned* discovery record
//! carrying the URLs, which would have preserved every existing
//! signature. It was rejected on two grounds:
//!
//! 1. **A URL decides who gets asked.** The state root already makes the
//!    *bytes* safe from any mirror ([`crate::location`]), so an unsigned
//!    URL cannot corrupt a node's state — but it can aim one. An unsigned
//!    side-channel lets any peer on the gossip mesh point every
//!    bootstrapping node in the cluster at a host of its choosing, which
//!    turns snapshot discovery into a reflected-load amplifier against a
//!    third party. Inside the signature, only the registry-authorized
//!    producer chooses.
//! 2. **Two records means two ways to disagree.** An unsigned record
//!    would need its own storage, its own merge rule when two peers
//!    advertise different locations for one snapshot id, its own
//!    expiry, and its own answer for "announced but no location yet".
//!    Each of those is a state a joining node can be caught in. One
//!    atomic signed record has none of them.
//!
//! The cost — a one-time break of every existing signature — is paid once
//! and only on a network where nothing depends on those signatures yet.
//! On a network where something did, this would need a versioned payload
//! and a verifier accepting both shapes.

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
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::SNAPSHOT_ANNOUNCE,
            &metadata,
        )
        .expect("SnapshotMetadata always serializes");
        Self {
            signature: keypair.sign(&bytes),
            metadata,
        }
    }

    pub fn verify(&self) -> Result<(), SnapshotError> {
        let expected = peer_id_from_public_key(&self.metadata.producer_public_key)
            .map_err(|_| SnapshotError::InvalidSignature)?;
        if expected != self.metadata.producer {
            return Err(SnapshotError::Unauthorized);
        }
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::SNAPSHOT_ANNOUNCE,
            &self.metadata,
        )
        .map_err(|_| SnapshotError::MalformedRecord)?;
        verify(&self.metadata.producer_public_key, &bytes, &self.signature)
            .map_err(|_| SnapshotError::InvalidSignature)
    }
}
