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
    /// The on-chain `Proposal` id this proposal claims, or `None`.
    ///
    /// **This field changes the signed transcript.** It is inside the
    /// bytes `SignedProposalCreate::sign` covers, which is the whole
    /// point — an off-chain claim on an on-chain proposal is only worth
    /// anything if the author signed it — but it also means a client that
    /// omits the field signs different bytes from one that includes it,
    /// and every node verifying will reject the mismatch. Clients, SDKs
    /// and nodes have to move together; see [`crate::onchain`].
    pub onchain_proposal_id: Option<u64>,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedProposalCreate {
    pub create: ProposalCreate,
    pub signature: Signature,
}

impl SignedProposalCreate {
    pub fn sign(create: ProposalCreate, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::json::to_bytes(&create)
            .expect("ProposalCreate always serializes");
        Self {
            signature: keypair.sign(&bytes),
            create,
        }
    }

    pub fn verify(&self) -> Result<(), GovernanceError> {
        let expected = peer_id_from_public_key(&self.create.author_public_key)
            .map_err(|_| GovernanceError::InvalidSignature)?;
        if expected != self.create.author {
            return Err(GovernanceError::Unauthorized);
        }
        let bytes = openfiat_serialization::json::to_bytes(&self.create)
            .map_err(|_| GovernanceError::MalformedProposal)?;
        verify(&self.create.author_public_key, &bytes, &self.signature)
            .map_err(|_| GovernanceError::InvalidSignature)
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VoteCast {
    pub proposal_id: ProposalId,
    pub voter: PeerId,
    pub voter_public_key: PublicKey,
    pub choice: VoteChoice,
    /// Self-reported, unverified. A node accepting a vote purely through
    /// this crate's own `apply_vote` trusts it as-is (this crate has no
    /// chain connectivity of its own); the real node
    /// (`crates/rpc::methods::governance::sendVoteCast`) instead defers
    /// to `apply_vote_with_verified_weight`, which ignores this field
    /// entirely in favor of `stake_account`'s independently-decoded
    /// on-chain amount.
    pub weight: u64,
    /// Base58 address of the `openfiat-staking` `StakeAccount` PDA this
    /// vote's weight is claimed from (seeds: `[b"stake", voter, role]` —
    /// OFS-4200 §5). Covered by this event's own signature (see
    /// `SignedVoteCast::verify`), so a voter can't have it swapped out
    /// after signing; the account itself is read and independently
    /// verified — see `crates/rpc::onchain_stake` — never trusted on the
    /// claim alone.
    pub stake_account: String,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedVoteCast {
    pub vote: VoteCast,
    pub signature: Signature,
}

impl SignedVoteCast {
    pub fn sign(vote: VoteCast, keypair: &Keypair) -> Self {
        let bytes =
            openfiat_serialization::json::to_bytes(&vote).expect("VoteCast always serializes");
        Self {
            signature: keypair.sign(&bytes),
            vote,
        }
    }

    pub fn verify(&self) -> Result<(), GovernanceError> {
        let expected = peer_id_from_public_key(&self.vote.voter_public_key)
            .map_err(|_| GovernanceError::InvalidSignature)?;
        if expected != self.vote.voter {
            return Err(GovernanceError::Unauthorized);
        }
        let bytes = openfiat_serialization::json::to_bytes(&self.vote)
            .map_err(|_| GovernanceError::MalformedProposal)?;
        verify(&self.vote.voter_public_key, &bytes, &self.signature)
            .map_err(|_| GovernanceError::InvalidSignature)
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
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::PROPOSAL_WITHDRAW,
            &withdraw,
        )
        .expect("ProposalWithdraw always serializes");
        Self {
            signature: keypair.sign(&bytes),
            withdraw,
        }
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
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::PROPOSAL_ACTIVATE,
            &activate,
        )
        .expect("ProposalActivate always serializes");
        Self {
            signature: keypair.sign(&bytes),
            activate,
        }
    }
}
