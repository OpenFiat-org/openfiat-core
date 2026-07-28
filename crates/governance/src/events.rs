//! Signed governance events (§24: "Every governance action MUST be
//! digitally signed"). `ProposalCreate` and `VoteCast` are self-
//! consistency verified; `ProposalWithdraw` and `ProposalActivate` are
//! verified against the proposal's on-file author key, the same
//! two-tier pattern used everywhere else in this workspace.

use crate::error::GovernanceError;
use crate::record::{ProposalCategory, ProposalId, VoteChoice};
use openfiat_crypto::{Keypair, verify};
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_types::{PeerId, PublicKey, Signature, Timestamp};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProposalCreate {
    pub id: ProposalId,
    pub title: String,
    pub summary: String,
    pub category: ProposalCategory,
    pub author: PeerId,
    pub author_public_key: PublicKey,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedProposalCreate {
    pub create: ProposalCreate,
    pub signature: Signature,
}

impl SignedProposalCreate {
    pub fn sign(create: ProposalCreate, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::wire::to_bytes(&create).expect("ProposalCreate always serializes");
        Self { signature: keypair.sign(&bytes), create }
    }

    pub fn verify(&self) -> Result<(), GovernanceError> {
        let expected = peer_id_from_public_key(&self.create.author_public_key).map_err(|_| GovernanceError::InvalidSignature)?;
        if expected != self.create.author {
            return Err(GovernanceError::Unauthorized);
        }
        let bytes = openfiat_serialization::wire::to_bytes(&self.create).map_err(|_| GovernanceError::MalformedProposal)?;
        verify(&self.create.author_public_key, &bytes, &self.signature).map_err(|_| GovernanceError::InvalidSignature)
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VoteCast {
    pub proposal_id: ProposalId,
    pub voter: PeerId,
    pub voter_public_key: PublicKey,
    pub choice: VoteChoice,
    pub weight: u64,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedVoteCast {
    pub vote: VoteCast,
    pub signature: Signature,
}

impl SignedVoteCast {
    pub fn sign(vote: VoteCast, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::wire::to_bytes(&vote).expect("VoteCast always serializes");
        Self { signature: keypair.sign(&bytes), vote }
    }

    pub fn verify(&self) -> Result<(), GovernanceError> {
        let expected = peer_id_from_public_key(&self.vote.voter_public_key).map_err(|_| GovernanceError::InvalidSignature)?;
        if expected != self.vote.voter {
            return Err(GovernanceError::Unauthorized);
        }
        let bytes = openfiat_serialization::wire::to_bytes(&self.vote).map_err(|_| GovernanceError::MalformedProposal)?;
        verify(&self.vote.voter_public_key, &bytes, &self.signature).map_err(|_| GovernanceError::InvalidSignature)
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProposalWithdraw {
    pub proposal_id: ProposalId,
    pub author: PeerId,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedProposalWithdraw {
    pub withdraw: ProposalWithdraw,
    pub signature: Signature,
}

impl SignedProposalWithdraw {
    pub fn sign(withdraw: ProposalWithdraw, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::wire::to_bytes(&withdraw).expect("ProposalWithdraw always serializes");
        Self { signature: keypair.sign(&bytes), withdraw }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProposalActivate {
    pub proposal_id: ProposalId,
    pub author: PeerId,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedProposalActivate {
    pub activate: ProposalActivate,
    pub signature: Signature,
}

impl SignedProposalActivate {
    pub fn sign(activate: ProposalActivate, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::wire::to_bytes(&activate).expect("ProposalActivate always serializes");
        Self { signature: keypair.sign(&bytes), activate }
    }
}
