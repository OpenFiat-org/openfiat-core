//! The replicated local governance index (§23: every governance action
//! is publicly recorded; §26: deterministic proposal states).

use crate::error::GovernanceError;
use crate::events::{
    SignedProposalActivate, SignedProposalCreate, SignedProposalWithdraw, SignedVoteCast,
};
use crate::protocol;
use crate::record::{CastVote, Proposal, ProposalId, ProposalStatus};
use openfiat_crypto::verify;
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{EventEnvelope, Timestamp};

const COLUMN_FAMILY: &str = "governance_proposals";

pub struct GovernanceRegistry<S> {
    store: S,
}

impl<S: KvStore> GovernanceRegistry<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }

    pub fn get(&self, id: &ProposalId) -> Option<Proposal> {
        let bytes = self
            .store
            .get(COLUMN_FAMILY, id.as_str().as_bytes())
            .ok()
            .flatten()?;
        wire::from_bytes(&bytes).ok()
    }

    fn put(&self, proposal: &Proposal) {
        if let Ok(bytes) = wire::to_bytes(proposal) {
            let _ = self
                .store
                .put(COLUMN_FAMILY, proposal.id.as_str().as_bytes(), &bytes);
        }
    }

    pub fn all(&self) -> Vec<Proposal> {
        self.store
            .iter_prefix(COLUMN_FAMILY, &[])
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(_, value)| wire::from_bytes(&value).ok())
            .collect()
    }

    pub fn apply_create(
        &self,
        signed: SignedProposalCreate,
    ) -> Result<ProposalId, GovernanceError> {
        signed.verify()?;
        let id = signed.create.id.clone();
        if self.get(&id).is_some() {
            return Err(GovernanceError::DuplicateProposalId);
        }
        let create = signed.create;
        let voting_closes_at = Timestamp::from_millis(
            create.timestamp.as_millis() + protocol::DEFAULT_VOTING_PERIOD.as_millis() as u64,
        );
        self.put(&Proposal {
            id: id.clone(),
            title: create.title,
            summary: create.summary,
            category: create.category,
            author: create.author,
            author_public_key: create.author_public_key,
            status: ProposalStatus::Voting,
            votes: Vec::new(),
            voting_closes_at,
            created_at: create.timestamp,
            updated_at: create.timestamp,
        });
        Ok(id)
    }

    /// §13/§24: only legal while voting is open, and only once per voter.
    pub fn apply_vote(&self, signed: SignedVoteCast) -> Result<(), GovernanceError> {
        signed.verify()?;
        let mut proposal = self
            .get(&signed.vote.proposal_id)
            .ok_or(GovernanceError::ProposalNotFound)?;
        if proposal.status != ProposalStatus::Voting
            || signed.vote.timestamp.as_millis() >= proposal.voting_closes_at.as_millis()
        {
            return Err(GovernanceError::VotingClosed);
        }
        if proposal.vote_by(&signed.vote.voter).is_some() {
            return Err(GovernanceError::DuplicateVote);
        }

        proposal.votes.push(CastVote {
            voter: signed.vote.voter,
            choice: signed.vote.choice,
            weight: signed.vote.weight,
            timestamp: signed.vote.timestamp,
        });
        proposal.updated_at = signed.vote.timestamp;
        self.put(&proposal);
        Ok(())
    }

    /// §21: proposal authors may withdraw before voting concludes.
    pub fn apply_withdraw(&self, signed: SignedProposalWithdraw) -> Result<(), GovernanceError> {
        let mut proposal = self
            .get(&signed.withdraw.proposal_id)
            .ok_or(GovernanceError::ProposalNotFound)?;
        if proposal.author != signed.withdraw.author {
            return Err(GovernanceError::Unauthorized);
        }
        let bytes =
            wire::to_bytes(&signed.withdraw).map_err(|_| GovernanceError::MalformedProposal)?;
        verify(&proposal.author_public_key, &bytes, &signed.signature)
            .map_err(|_| GovernanceError::InvalidSignature)?;
        if proposal.status != ProposalStatus::Voting {
            return Err(GovernanceError::InvalidStateTransition);
        }

        proposal.status = ProposalStatus::Withdrawn;
        proposal.updated_at = signed.withdraw.timestamp;
        self.put(&proposal);
        Ok(())
    }

    /// §18: only a proposal that reached `Accepted` may be activated.
    pub fn apply_activate(&self, signed: SignedProposalActivate) -> Result<(), GovernanceError> {
        let mut proposal = self
            .get(&signed.activate.proposal_id)
            .ok_or(GovernanceError::ProposalNotFound)?;
        if proposal.author != signed.activate.author {
            return Err(GovernanceError::Unauthorized);
        }
        let bytes =
            wire::to_bytes(&signed.activate).map_err(|_| GovernanceError::MalformedProposal)?;
        verify(&proposal.author_public_key, &bytes, &signed.signature)
            .map_err(|_| GovernanceError::InvalidSignature)?;
        if proposal.status != ProposalStatus::Accepted {
            return Err(GovernanceError::InvalidStateTransition);
        }

        proposal.status = ProposalStatus::Activated;
        proposal.updated_at = signed.activate.timestamp;
        self.put(&proposal);
        Ok(())
    }

    /// §15-16: every node independently resolves proposals whose voting
    /// window has passed, purely from timestamps and votes it already
    /// has — no gossip event required, the same local-bookkeeping
    /// approach reservations' `expire_stale` uses.
    pub fn resolve_expired(&self, now: Timestamp) -> usize {
        let mut resolved = 0;
        for mut proposal in self.all() {
            if proposal.status != ProposalStatus::Voting
                || now.as_millis() < proposal.voting_closes_at.as_millis()
            {
                continue;
            }
            proposal.status = tally(&proposal);
            proposal.updated_at = now;
            self.put(&proposal);
            resolved += 1;
        }
        resolved
    }

    pub fn apply_event(&self, event: &EventEnvelope) {
        if event.ofs_spec != protocol::OFS_SPEC {
            return;
        }
        match event.event_type.as_str() {
            protocol::EVENT_CREATED => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_create(signed);
                }
            }
            protocol::EVENT_VOTE_CAST => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_vote(signed);
                }
            }
            protocol::EVENT_WITHDRAWN => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_withdraw(signed);
                }
            }
            protocol::EVENT_ACTIVATED => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_activate(signed);
                }
            }
            _ => {}
        }
    }
}

/// §15-16: quorum (a minimum number of distinct voters) and a simple
/// majority of weight among `Approve`/`Reject` votes; a quorum miss or a
/// genuine weight tie both resolve to `Rejected` as a safe, deterministic
/// fallback, the same tie-breaking philosophy as disputes' consensus.
fn tally(proposal: &Proposal) -> ProposalStatus {
    if proposal.votes.len() < protocol::MINIMUM_VOTERS_FOR_QUORUM {
        return ProposalStatus::Rejected;
    }
    let (mut approve, mut reject) = (0u64, 0u64);
    for vote in &proposal.votes {
        match vote.choice {
            crate::record::VoteChoice::Approve => approve += vote.weight,
            crate::record::VoteChoice::Reject => reject += vote.weight,
            crate::record::VoteChoice::Abstain => {}
        }
    }
    if approve > reject {
        ProposalStatus::Accepted
    } else {
        ProposalStatus::Rejected
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{ProposalActivate, ProposalCreate, ProposalWithdraw, VoteCast};
    use crate::record::{ProposalCategory, VoteChoice};
    use openfiat_crypto::Keypair;
    use openfiat_network::identity::peer_id_from_public_key;
    use openfiat_storage::mem::MemoryStore;

    fn registry_with_proposal(
        author: &Keypair,
        id: &str,
    ) -> (GovernanceRegistry<MemoryStore>, ProposalId) {
        let registry = GovernanceRegistry::new(MemoryStore::new());
        let create = ProposalCreate {
            id: ProposalId::new(id),
            title: "Increase Reservation Timeout".to_string(),
            summary: "Raise the validation window from 30 to 45 minutes.".to_string(),
            category: ProposalCategory::Protocol,
            author: peer_id_from_public_key(&author.public_key()).unwrap(),
            author_public_key: author.public_key(),
            timestamp: Timestamp::now(),
        };
        let id = registry
            .apply_create(SignedProposalCreate::sign(create, author))
            .unwrap();
        (registry, id)
    }

    fn cast(
        registry: &GovernanceRegistry<MemoryStore>,
        proposal_id: &ProposalId,
        voter: &Keypair,
        choice: VoteChoice,
        weight: u64,
    ) -> Result<(), GovernanceError> {
        let vote = VoteCast {
            proposal_id: proposal_id.clone(),
            voter: peer_id_from_public_key(&voter.public_key()).unwrap(),
            voter_public_key: voter.public_key(),
            choice,
            weight,
            timestamp: Timestamp::now(),
        };
        registry.apply_vote(SignedVoteCast::sign(vote, voter))
    }

    #[test]
    fn a_created_proposal_opens_directly_for_voting() {
        let author = Keypair::generate();
        let (registry, id) = registry_with_proposal(&author, "ofp-1");
        assert_eq!(registry.get(&id).unwrap().status, ProposalStatus::Voting);
    }

    #[test]
    fn duplicate_creation_is_rejected() {
        let author = Keypair::generate();
        let registry = GovernanceRegistry::new(MemoryStore::new());
        let create = || ProposalCreate {
            id: ProposalId::new("ofp-1"),
            title: "T".to_string(),
            summary: "S".to_string(),
            category: ProposalCategory::Protocol,
            author: peer_id_from_public_key(&author.public_key()).unwrap(),
            author_public_key: author.public_key(),
            timestamp: Timestamp::now(),
        };
        registry
            .apply_create(SignedProposalCreate::sign(create(), &author))
            .unwrap();
        let result = registry.apply_create(SignedProposalCreate::sign(create(), &author));
        assert_eq!(result, Err(GovernanceError::DuplicateProposalId));
    }

    #[test]
    fn a_voter_cannot_vote_twice() {
        let author = Keypair::generate();
        let (registry, id) = registry_with_proposal(&author, "ofp-1");
        let voter = Keypair::generate();
        cast(&registry, &id, &voter, VoteChoice::Approve, 10).unwrap();
        let result = cast(&registry, &id, &voter, VoteChoice::Reject, 10);
        assert_eq!(result, Err(GovernanceError::DuplicateVote));
    }

    #[test]
    fn resolution_requires_quorum_and_majority() {
        let author = Keypair::generate();
        let (registry, id) = registry_with_proposal(&author, "ofp-1");
        let voters: Vec<Keypair> = (0..3).map(|_| Keypair::generate()).collect();
        cast(&registry, &id, &voters[0], VoteChoice::Approve, 10).unwrap();
        cast(&registry, &id, &voters[1], VoteChoice::Approve, 5).unwrap();
        cast(&registry, &id, &voters[2], VoteChoice::Reject, 3).unwrap();

        let far_future =
            Timestamp::from_millis(registry.get(&id).unwrap().voting_closes_at.as_millis() + 1);
        assert_eq!(registry.resolve_expired(far_future), 1);
        assert_eq!(registry.get(&id).unwrap().status, ProposalStatus::Accepted);
    }

    #[test]
    fn a_quorum_miss_resolves_to_rejected() {
        let author = Keypair::generate();
        let (registry, id) = registry_with_proposal(&author, "ofp-1");
        let voter = Keypair::generate();
        cast(&registry, &id, &voter, VoteChoice::Approve, 1000).unwrap();

        let far_future =
            Timestamp::from_millis(registry.get(&id).unwrap().voting_closes_at.as_millis() + 1);
        registry.resolve_expired(far_future);
        assert_eq!(registry.get(&id).unwrap().status, ProposalStatus::Rejected);
    }

    #[test]
    fn withdrawal_by_a_non_author_is_rejected() {
        let author = Keypair::generate();
        let (registry, id) = registry_with_proposal(&author, "ofp-1");
        let stranger = Keypair::generate();
        let withdraw = ProposalWithdraw {
            proposal_id: id,
            author: peer_id_from_public_key(&stranger.public_key()).unwrap(),
            timestamp: Timestamp::now(),
        };
        let result = registry.apply_withdraw(SignedProposalWithdraw::sign(withdraw, &stranger));
        assert_eq!(result, Err(GovernanceError::Unauthorized));
    }

    #[test]
    fn activation_requires_accepted_status() {
        let author = Keypair::generate();
        let (registry, id) = registry_with_proposal(&author, "ofp-1");
        let author_peer_id = peer_id_from_public_key(&author.public_key()).unwrap();
        let activate = ProposalActivate {
            proposal_id: id,
            author: author_peer_id,
            timestamp: Timestamp::now(),
        };
        let result = registry.apply_activate(SignedProposalActivate::sign(activate, &author));
        assert_eq!(result, Err(GovernanceError::InvalidStateTransition));
    }

    #[test]
    fn the_full_lifecycle_reaches_activated() {
        let author = Keypair::generate();
        let (registry, id) = registry_with_proposal(&author, "ofp-1");
        let voters: Vec<Keypair> = (0..3).map(|_| Keypair::generate()).collect();
        for voter in &voters {
            cast(&registry, &id, voter, VoteChoice::Approve, 1).unwrap();
        }
        let far_future =
            Timestamp::from_millis(registry.get(&id).unwrap().voting_closes_at.as_millis() + 1);
        registry.resolve_expired(far_future);
        assert_eq!(registry.get(&id).unwrap().status, ProposalStatus::Accepted);

        let author_peer_id = peer_id_from_public_key(&author.public_key()).unwrap();
        let activate = ProposalActivate {
            proposal_id: id.clone(),
            author: author_peer_id,
            timestamp: Timestamp::now(),
        };
        registry
            .apply_activate(SignedProposalActivate::sign(activate, &author))
            .unwrap();
        assert_eq!(registry.get(&id).unwrap().status, ProposalStatus::Activated);
    }
}
