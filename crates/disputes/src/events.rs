//! Signed dispute events. `DisputeOpen` and `ArbitratorJoin` are
//! self-consistency verified (the acting party's claimed identity must
//! match their embedded public key); `VoteCommit`/`VoteReveal`/
//! `MutualSettlementAgree` are verified against whichever key is already
//! on file for that dispute (the arbitrator's from their join record, or
//! the buyer's/seller's from the referenced settlement) — the same
//! two-tier pattern used by every other signed action in this workspace.

use crate::error::DisputeError;
use crate::record::{DisputeId, Vote};
use openfiat_crypto::{Keypair, verify};
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_settlement::SettlementId;
use openfiat_types::{PeerId, PublicKey, Signature, Timestamp};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DisputeOpen {
    pub id: DisputeId,
    pub settlement_id: SettlementId,
    pub opener: PeerId,
    pub opener_public_key: PublicKey,
    pub reason: String,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedDisputeOpen {
    pub open: DisputeOpen,
    pub signature: Signature,
}

impl SignedDisputeOpen {
    pub fn sign(open: DisputeOpen, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::DISPUTE_OPEN,
            &open,
        )
        .expect("DisputeOpen always serializes");
        Self {
            signature: keypair.sign(&bytes),
            open,
        }
    }

    pub fn verify(&self) -> Result<(), DisputeError> {
        let expected = peer_id_from_public_key(&self.open.opener_public_key)
            .map_err(|_| DisputeError::InvalidSignature)?;
        if expected != self.open.opener {
            return Err(DisputeError::Unauthorized);
        }
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::DISPUTE_OPEN,
            &self.open,
        )
        .map_err(|_| DisputeError::MalformedDispute)?;
        verify(&self.open.opener_public_key, &bytes, &self.signature)
            .map_err(|_| DisputeError::InvalidSignature)
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ArbitratorJoin {
    pub dispute_id: DisputeId,
    pub arbitrator: PeerId,
    pub arbitrator_public_key: PublicKey,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedArbitratorJoin {
    pub join: ArbitratorJoin,
    pub signature: Signature,
}

impl SignedArbitratorJoin {
    pub fn sign(join: ArbitratorJoin, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::ARBITRATOR_JOIN,
            &join,
        )
        .expect("ArbitratorJoin always serializes");
        Self {
            signature: keypair.sign(&bytes),
            join,
        }
    }

    pub fn verify(&self) -> Result<(), DisputeError> {
        let expected = peer_id_from_public_key(&self.join.arbitrator_public_key)
            .map_err(|_| DisputeError::InvalidSignature)?;
        if expected != self.join.arbitrator {
            return Err(DisputeError::Unauthorized);
        }
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::ARBITRATOR_JOIN,
            &self.join,
        )
        .map_err(|_| DisputeError::MalformedDispute)?;
        verify(&self.join.arbitrator_public_key, &bytes, &self.signature)
            .map_err(|_| DisputeError::InvalidSignature)
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VoteCommit {
    pub dispute_id: DisputeId,
    pub arbitrator: PeerId,
    pub commitment: [u8; 32],
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedVoteCommit {
    pub commit: VoteCommit,
    pub signature: Signature,
}

impl SignedVoteCommit {
    pub fn sign(commit: VoteCommit, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::DISPUTE_VOTE_COMMIT,
            &commit,
        )
        .expect("VoteCommit always serializes");
        Self {
            signature: keypair.sign(&bytes),
            commit,
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VoteReveal {
    pub dispute_id: DisputeId,
    pub arbitrator: PeerId,
    pub vote: Vote,
    pub secret: [u8; 32],
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedVoteReveal {
    pub reveal: VoteReveal,
    pub signature: Signature,
}

impl SignedVoteReveal {
    pub fn sign(reveal: VoteReveal, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::DISPUTE_VOTE_REVEAL,
            &reveal,
        )
        .expect("VoteReveal always serializes");
        Self {
            signature: keypair.sign(&bytes),
            reveal,
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MutualSettlementAgree {
    pub dispute_id: DisputeId,
    pub party: PeerId,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedMutualSettlementAgree {
    pub agree: MutualSettlementAgree,
    pub signature: Signature,
}

impl SignedMutualSettlementAgree {
    pub fn sign(agree: MutualSettlementAgree, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::MUTUAL_SETTLEMENT_AGREE,
            &agree,
        )
        .expect("MutualSettlementAgree always serializes");
        Self {
            signature: keypair.sign(&bytes),
            agree,
        }
    }
}
